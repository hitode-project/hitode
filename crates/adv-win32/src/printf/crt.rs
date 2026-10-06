//! VC80/90 buffer formatting over checked x86 slots, independent of WINAPIV.
use super::{Unit, pad_num, pad_units, units};
use adv_abi::GuestPtr;
use adv_cpu::backend::{Guest, MemFault};
use adv_os::Codec;

/// Output, format, string scans and numeric fields have the same explicit bound.
pub(crate) const LIMIT: usize = 65536;
#[derive(Debug)]
pub(crate) enum Error {
    Memory(MemFault),
    Invalid(&'static str),
    Unsupported(&'static str),
    Encoding,
}
impl From<MemFault> for Error {
    fn from(error: MemFault) -> Self {
        Self::Memory(error)
    }
}
struct Arguments<'a> {
    cpu: &'a dyn Guest,
    pointer: Option<u32>,
}
impl Arguments<'_> {
    fn next(&mut self) -> Result<u32, Error> {
        let pointer = self.pointer.ok_or(Error::Memory(MemFault {
            addr: u32::MAX,
            len: 4,
        }))?;
        let result = GuestPtr::<u32>::new(pointer).read(self.cpu)?;
        self.pointer = pointer.checked_add(4);
        Ok(result)
    }
    fn value(&mut self, quad: bool) -> Result<u64, Error> {
        let low = u64::from(self.next()?);
        Ok(if quad {
            low | u64::from(self.next()?) << 32
        } else {
            low
        })
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Length {
    Normal,
    Short,
    Long,
    Quad,
    Wide,
    Floating,
}
#[derive(Clone, Copy)]
enum Field {
    Number(usize),
    Argument,
}
fn number<T: Unit>(fmt: &[T], at: &mut usize) -> Result<Field, Error> {
    if fmt.get(*at) == Some(&T::from(b'*')) {
        *at += 1;
        return Ok(Field::Argument);
    }
    let mut value = 0usize;
    while let Some(unit) = fmt.get(*at).filter(|u| u.byte().is_ascii_digit()) {
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(usize::from(unit.byte() - b'0')))
            .filter(|v| *v <= LIMIT)
            .ok_or(Error::Unsupported(
                "numeric field exceeds the bounded formatter profile",
            ))?;
        *at += 1;
    }
    Ok(Field::Number(value))
}
fn source_unit<T: super::ReadUnit>(
    cpu: &dyn Guest,
    pointer: u32,
    index: usize,
) -> Result<T, Error> {
    let address = index
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|n| u32::try_from(n).ok())
        .and_then(|offset| pointer.checked_add(offset))
        .ok_or(Error::Memory(MemFault {
            addr: pointer,
            len: std::mem::size_of::<T>() as u32,
        }))?;
    Ok(T::read(cpu, address)?)
}
fn append<T: Unit>(out: &mut Vec<T>, text: &[T]) -> Result<(), Error> {
    if out.len().checked_add(text.len()).is_none_or(|n| n > LIMIT) {
        return Err(Error::Unsupported(
            "output exceeds the bounded formatter profile",
        ));
    }
    out.extend_from_slice(text);
    Ok(())
}
fn narrow_character<T: Unit>(codec: &Codec, cp: u32, bytes: &[u8]) -> Result<Vec<T>, Error> {
    if !T::WIDE {
        return Ok(T::narrow(codec, bytes));
    }
    let wide = if cp == 0 {
        bytes.iter().map(|v| u16::from(*v)).collect()
    } else {
        let (wide, replaced) = codec.to_utf16_checked(cp, bytes);
        if replaced {
            return Err(Error::Encoding);
        }
        wide
    };
    Ok(T::wide(codec, &wide))
}
fn wide_character<T: Unit>(codec: &Codec, cp: u32, wide: &[u16]) -> Result<Vec<T>, Error> {
    if wide.contains(&0xffff) {
        return Err(Error::Encoding);
    }
    if T::WIDE {
        return Ok(T::wide(codec, wide));
    }
    let bytes = if cp == 0 {
        if wide.iter().any(|v| *v > 255) {
            return Err(Error::Encoding);
        }
        wide.iter().map(|v| *v as u8).collect()
    } else {
        let (bytes, replaced) = codec.from_utf16(cp, wide);
        if replaced {
            return Err(Error::Encoding);
        }
        bytes
    };
    Ok(T::narrow(codec, &bytes))
}
fn string<T: Unit>(
    cpu: &dyn Guest,
    codec: &Codec,
    cp: u32,
    pointer: u32,
    wide: bool,
    precision: Option<usize>,
) -> Result<Vec<T>, Error> {
    if pointer == 0 {
        let mut text = units::<T>(b"(null)");
        if let Some(p) = precision {
            text.truncate(p);
        }
        return Ok(text);
    }
    let limit = precision.unwrap_or(LIMIT);
    let mut text = Vec::new();
    let mut consumed = 0;
    for _ in 0..limit {
        if wide {
            let value = source_unit::<u16>(cpu, pointer, consumed)?;
            if value == 0 {
                return Ok(text);
            }
            consumed += 1;
            // VC90 narrow %ls precision counts source UTF-16 units; a DBCS
            // conversion can produce more bytes than this precision.
            append(&mut text, &wide_character(codec, cp, &[value])?)?;
        } else {
            let value = source_unit::<u8>(cpu, pointer, consumed)?;
            if value == 0 {
                return Ok(text);
            }
            consumed += 1;
            let mut bytes = vec![value];
            if T::WIDE && cp != 0 && codec.is_lead_byte(cp, value) {
                let trail = source_unit::<u8>(cpu, pointer, consumed)?;
                if trail == 0 {
                    return Err(Error::Encoding);
                }
                bytes.push(trail);
                consumed += 1;
            }
            append(&mut text, &narrow_character(codec, cp, &bytes)?)?;
        }
    }
    if precision.is_none() {
        let ended = if wide {
            source_unit::<u16>(cpu, pointer, consumed)? == 0
        } else {
            source_unit::<u8>(cpu, pointer, consumed)? == 0
        };
        if !ended {
            return Err(Error::Unsupported(
                "string scan exceeds the bounded formatter profile",
            ));
        }
    }
    Ok(text)
}
fn render<T: Unit>(
    cpu: &dyn Guest,
    cp: u32,
    fmt: &[T],
    pointer: u32,
) -> Result<(Vec<T>, bool), Error> {
    if !matches!(cp, 0 | 932 | 936 | 1252) {
        return Err(Error::Unsupported(
            "CRT LC_CTYPE code page is outside the formatting profile",
        ));
    }
    let codec = Codec::new(if cp == 0 { 1252 } else { cp });
    let mut args = Arguments {
        cpu,
        pointer: Some(pointer),
    };
    let mut out = Vec::new();
    let mut encoding_error = false;
    let mut at = 0;
    while at < fmt.len() {
        if fmt[at] != T::from(b'%') {
            let count = if !T::WIDE
                && cp != 0
                && codec.is_lead_byte(cp, fmt[at].byte())
                && at + 1 < fmt.len()
            {
                2
            } else {
                1
            };
            append(&mut out, &fmt[at..at + count])?;
            at += count;
            continue;
        }
        at += 1;
        if fmt.get(at) == Some(&T::from(b'%')) {
            append(&mut out, &units(b"%"))?;
            at += 1;
            continue;
        }
        let (mut left, mut zero, mut plus, mut space, mut alternate) =
            (false, false, false, false, false);
        while let Some(unit) = fmt.get(at) {
            match unit.byte() {
                b'-' => left = true,
                b'0' => zero = true,
                b'+' => plus = true,
                b' ' => space = true,
                b'#' => alternate = true,
                _ => break,
            }
            at += 1;
        }
        let width = number(fmt, &mut at)?;
        let precision = if fmt.get(at) == Some(&T::from(b'.')) {
            at += 1;
            Some(number(fmt, &mut at)?)
        } else {
            None
        };
        let length = match fmt.get(at).map(|v| v.byte()) {
            Some(b'h') => {
                at += 1;
                if fmt.get(at) == Some(&T::from(b'h')) {
                    at += 1;
                }
                Length::Short
            }
            Some(b'l') => {
                at += 1;
                if fmt.get(at) == Some(&T::from(b'l')) {
                    at += 1;
                    Length::Quad
                } else {
                    Length::Long
                }
            }
            Some(b'w') => {
                at += 1;
                Length::Wide
            }
            Some(b'L') => {
                at += 1;
                Length::Floating
            }
            Some(b'I') => {
                at += 1;
                if fmt.get(at) == Some(&T::from(b'6')) && fmt.get(at + 1) == Some(&T::from(b'4')) {
                    at += 2;
                    Length::Quad
                } else {
                    if fmt.get(at) == Some(&T::from(b'3'))
                        && fmt.get(at + 1) == Some(&T::from(b'2'))
                    {
                        at += 2;
                    }
                    Length::Long
                }
            }
            _ => Length::Normal,
        };
        let conversion = fmt
            .get(at)
            .ok_or(Error::Invalid("unterminated format conversion"))?
            .byte();
        at += 1;
        // Classify before reading any conversion value: rejected formats never
        // probe invented argument slots or substitute placeholder strings.
        if matches!(
            conversion,
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A'
        ) {
            return Err(Error::Unsupported(
                "floating-point CRT formatting is unavailable",
            ));
        }
        if conversion == b'n' {
            return Err(Error::Invalid("%n count-output is disabled"));
        }
        if conversion == b'Z' {
            return Err(Error::Unsupported(
                "counted-string CRT formatting is unavailable",
            ));
        }
        if !matches!(
            conversion,
            b'd' | b'i' | b'u' | b'o' | b'x' | b'X' | b'p' | b's' | b'S' | b'c' | b'C'
        ) {
            return Err(Error::Invalid(
                "format conversion is not valid in the VC80/90 profile",
            ));
        }
        if length == Length::Floating
            || length == Length::Wide && !matches!(conversion, b'c' | b'C' | b's' | b'S')
            || length == Length::Quad && matches!(conversion, b'c' | b'C' | b's' | b'S' | b'p')
        {
            return Err(Error::Invalid(
                "format length modifier is incompatible with conversion",
            ));
        }
        let width = match width {
            Field::Number(value) => value,
            Field::Argument => {
                let value = args.next()? as i32;
                left |= value < 0;
                value.unsigned_abs() as usize
            }
        };
        let precision = match precision {
            Some(Field::Number(value)) => Some(value),
            Some(Field::Argument) => {
                let value = args.next()? as i32;
                (value >= 0).then_some(value as usize)
            }
            None => None,
        };
        if width > LIMIT || precision.is_some_and(|v| v > LIMIT) {
            return Err(Error::Unsupported(
                "dynamic field exceeds the bounded formatter profile",
            ));
        }
        let body = match conversion {
            b'c' | b'C' | b's' | b'S' => {
                let wide = match length {
                    Length::Long | Length::Wide => true,
                    Length::Short => false,
                    _ => T::WIDE == matches!(conversion, b'c' | b's'),
                };
                let value = args.next()?;
                let text = if matches!(conversion, b's' | b'S') {
                    string(cpu, &codec, cp, value, wide, precision)?
                } else {
                    let character = if wide {
                        wide_character(&codec, cp, &[value as u16])
                    } else {
                        narrow_character(&codec, cp, &[value as u8])
                    };
                    match character {
                        Ok(text) => text,
                        // Unlike strings, a c/C encoding error skips the whole
                        // field and lets the remaining format run (VC90 oracle).
                        Err(Error::Encoding) => {
                            encoding_error = true;
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                };
                pad_units(&text, width, left)
            }
            b'p' => units(&pad_num(
                "",
                &format!("{:08X}", args.next()?),
                width,
                left,
                zero,
            )),
            _ => {
                let value = args.value(length == Length::Quad)?;
                let signed = matches!(conversion, b'd' | b'i');
                let sign_value = if length == Length::Quad {
                    value as i64
                } else if length == Length::Short {
                    i64::from(value as i16)
                } else {
                    i64::from(value as i32)
                };
                let magnitude = if signed {
                    sign_value.unsigned_abs()
                } else if length == Length::Short {
                    u64::from(value as u16)
                } else {
                    value
                };
                let mut digits = if precision == Some(0) && magnitude == 0 {
                    String::new()
                } else {
                    match conversion {
                        b'x' => format!("{magnitude:x}"),
                        b'X' => format!("{magnitude:X}"),
                        b'o' => format!("{magnitude:o}"),
                        _ => magnitude.to_string(),
                    }
                };
                if let Some(p) = precision {
                    if digits.len() < p {
                        digits = "0".repeat(p - digits.len()) + &digits;
                    }
                }
                if alternate && conversion == b'o' && !digits.starts_with('0') {
                    digits.insert(0, '0');
                }
                let prefix = if signed {
                    if sign_value < 0 {
                        "-"
                    } else if plus {
                        "+"
                    } else if space {
                        " "
                    } else {
                        ""
                    }
                } else if alternate && magnitude != 0 {
                    match conversion {
                        b'x' => "0x",
                        b'X' => "0X",
                        _ => "",
                    }
                } else {
                    ""
                };
                units(&pad_num(
                    prefix,
                    &digits,
                    width,
                    left,
                    zero && precision.is_none(),
                ))
            }
        };
        append(&mut out, &body)?;
    }
    Ok((out, encoding_error))
}
pub(crate) fn narrow(
    cpu: &dyn Guest,
    cp: u32,
    fmt: &[u8],
    arguments: u32,
) -> Result<(Vec<u8>, bool), Error> {
    render(cpu, cp, fmt, arguments)
}
pub(crate) fn wide(
    cpu: &dyn Guest,
    cp: u32,
    fmt: &[u16],
    arguments: u32,
) -> Result<(Vec<u16>, bool), Error> {
    render(cpu, cp, fmt, arguments)
}
