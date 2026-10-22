//! Byte encodings have no provider or network dependency. ASCII syntax is
//! parsed before the guest buffers are published; Base64 follows RFC 4648.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const HEX: &[u8; 16] = b"0123456789abcdef";
const INVALID_DATA: u32 = 13;
const INVALID_PARAMETER: u32 = 87;
fn newline(flags: u32) -> &'static [u8] {
    if flags & 0x40000000 != 0 {
        b""
    } else if flags & 0x80000000 != 0 {
        b"\n"
    } else {
        b"\r\n"
    }
}
pub(super) fn encode(input: &[u8], flags: u32) -> Result<Vec<u8>, u32> {
    if flags & !0xc00000ff != 0 {
        return Err(INVALID_PARAMETER);
    }
    let format = flags & 0xff;
    let ending = newline(flags);
    let mut out = Vec::new();
    match format {
        0 | 1 | 3 | 9 => {
            let label = match format {
                0 => Some("CERTIFICATE"),
                3 => Some("NEW CERTIFICATE REQUEST"),
                9 => Some("X509 CRL"),
                _ => None,
            };
            if let Some(label) = label {
                out.extend_from_slice(format!("-----BEGIN {label}-----").as_bytes());
                out.extend_from_slice(ending);
            }
            let mut column = 0;
            for chunk in input.chunks(3) {
                let value = (u32::from(chunk[0]) << 16)
                    | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                    | u32::from(*chunk.get(2).unwrap_or(&0));
                out.push(ALPHABET[(value >> 18) as usize]);
                out.push(ALPHABET[((value >> 12) & 63) as usize]);
                out.push(if chunk.len() > 1 {
                    ALPHABET[((value >> 6) & 63) as usize]
                } else {
                    b'='
                });
                out.push(if chunk.len() > 2 {
                    ALPHABET[(value & 63) as usize]
                } else {
                    b'='
                });
                column += 4;
                if column == 64 {
                    out.extend_from_slice(ending);
                    column = 0;
                }
            }
            if column != 0 {
                out.extend_from_slice(ending);
            }
            if let Some(label) = label {
                out.extend_from_slice(format!("-----END {label}-----").as_bytes());
                out.extend_from_slice(ending);
            }
        }
        2 => out.extend_from_slice(input),
        4 | 5 | 10 | 11 => {
            if format == 4 && ending.is_empty() {
                for (index, &byte) in input.iter().enumerate() {
                    if index != 0 {
                        out.push(b' ');
                    }
                    out.push(HEX[(byte >> 4) as usize]);
                    out.push(HEX[(byte & 15) as usize]);
                }
                return Ok(out);
            }
            let ascii = matches!(format, 5 | 11);
            let address = matches!(format, 10 | 11);
            for (line, chunk) in input.chunks(16).enumerate() {
                if address {
                    out.extend_from_slice(format!("{:04x}\t", line * 16).as_bytes());
                }
                for (index, &byte) in chunk.iter().enumerate() {
                    if index != 0 {
                        out.push(b' ');
                        if index == 8 {
                            out.push(b' ');
                        }
                    }
                    out.push(HEX[(byte >> 4) as usize]);
                    out.push(HEX[(byte & 15) as usize]);
                }
                if ascii {
                    let padding = (16 - chunk.len()) * 3 + usize::from(chunk.len() <= 8) + 3;
                    out.resize(out.len() + padding, b' ');
                    out.extend(chunk.iter().map(|&byte| {
                        if (32..127).contains(&byte) {
                            byte
                        } else {
                            b'.'
                        }
                    }));
                }
                out.extend_from_slice(ending);
            }
        }
        12 => {
            for &byte in input {
                out.push(HEX[(byte >> 4) as usize]);
                out.push(HEX[(byte & 15) as usize]);
            }
            out.extend_from_slice(ending);
        }
        _ => return Err(INVALID_PARAMETER),
    }
    Ok(out)
}
fn white(unit: u16) -> bool {
    matches!(unit, 9 | 10 | 13 | 32)
}
fn sextet(unit: u16) -> Option<u32> {
    Some(match unit {
        65..=90 => u32::from(unit - 65),
        97..=122 => u32::from(unit - 97 + 26),
        48..=57 => u32::from(unit - 48 + 52),
        43 => 62,
        47 => 63,
        _ => return None,
    })
}
fn base64(input: &[u16], strict: bool) -> Result<Vec<u8>, u32> {
    let mut out = Vec::new();
    let mut bits = 0u32;
    let mut available = 0;
    let mut digits = 0usize;
    let mut padding = 0;
    for &unit in input {
        if white(unit) {
            continue;
        }
        if unit == 61 {
            if digits == 0 {
                return Err(INVALID_DATA);
            }
            padding += 1;
            if !strict {
                break;
            }
            if padding > 3 {
                return Err(INVALID_DATA);
            }
            continue;
        }
        if padding != 0 {
            return Err(INVALID_DATA);
        }
        let digit = match sextet(unit) {
            Some(digit) => digit,
            None if !strict && digits != 0 => break,
            None => return Err(INVALID_DATA),
        };
        bits = (bits << 6) | digit;
        available += 6;
        digits += 1;
        if available >= 8 {
            available -= 8;
            out.push((bits >> available) as u8);
        }
    }
    if digits % 4 == 1 {
        // Native zero-terminated controls accept a final single sextet and
        // preserve bit 2 in the resulting byte, including in strict mode.
        out.push((bits << (8 - available)) as u8 | 4);
    }
    Ok(out)
}
fn find(input: &[u16], needle: &[u8]) -> Option<usize> {
    input.windows(needle.len()).position(|window| {
        window
            .iter()
            .copied()
            .eq(needle.iter().copied().map(u16::from))
    })
}
fn header(input: &[u16], strict: bool) -> Result<(Vec<u8>, u32), u32> {
    let skip = find(input, b"-----BEGIN ").ok_or(INVALID_DATA)?;
    let start = skip + 11;
    let start = start + find(&input[start..], b"-----").ok_or(INVALID_DATA)? + 5;
    let end = start + find(&input[start..], b"-----END ").ok_or(INVALID_DATA)?;
    let trailer = end + 9;
    find(&input[trailer..], b"-----").ok_or(INVALID_DATA)?;
    Ok((base64(&input[start..end], strict)?, skip as u32))
}
fn nibble(unit: u16) -> Option<u8> {
    match unit {
        48..=57 => Some((unit - 48) as u8),
        65..=70 => Some((unit - 65 + 10) as u8),
        97..=102 => Some((unit - 97 + 10) as u8),
        _ => None,
    }
}
fn hex(input: &[u16], format: u32) -> Result<Vec<u8>, u32> {
    if matches!(format, 4 | 12) {
        let mut out = Vec::new();
        let mut high = None;
        for &unit in input {
            if white(unit) {
                continue;
            }
            let digit = nibble(unit).ok_or(INVALID_DATA)?;
            if let Some(value) = high.take() {
                out.push(value * 16 + digit);
            } else {
                high = Some(digit);
            }
        }
        return if high.is_none() {
            Ok(out)
        } else {
            Err(INVALID_DATA)
        };
    }
    let mut out = Vec::new();
    let mut remaining = input;
    while !remaining.is_empty() {
        let whitespace = remaining.iter().take_while(|&&unit| white(unit)).count();
        remaining = &remaining[whitespace..];
        if remaining.is_empty() {
            break;
        }
        if matches!(format, 10 | 11) {
            let end = remaining
                .iter()
                .position(|unit| white(*unit))
                .ok_or(INVALID_DATA)?;
            if end < 4 || !remaining[..end].iter().all(|unit| nibble(*unit).is_some()) {
                return Err(INVALID_DATA);
            }
            remaining = &remaining[end..];
        }
        let mut count = 0;
        while !remaining.is_empty() && count < 16 {
            let spaces = remaining
                .iter()
                .take_while(|&&unit| matches!(unit, 9 | 32))
                .count();
            if count != 0 && matches!(format, 5 | 11) && spaces >= 3 {
                break;
            }
            if count != 0 && spaces == 0 && !matches!(remaining[0], 10 | 13) {
                return Err(INVALID_DATA);
            }
            remaining = &remaining[spaces..];
            if remaining.is_empty() || matches!(remaining[0], 10 | 13) {
                break;
            }
            if remaining.len() < 2 {
                return Err(INVALID_DATA);
            }
            let high = nibble(remaining[0]).ok_or(INVALID_DATA)?;
            let low = nibble(remaining[1]).ok_or(INVALID_DATA)?;
            out.push(high * 16 + low);
            remaining = &remaining[2..];
            count += 1;
        }
        if count == 0 {
            return Err(INVALID_DATA);
        }
        if matches!(format, 5 | 11) {
            if remaining.iter().all(|&unit| white(unit)) {
                break;
            }
            let padding = (16 - count) * 3 + usize::from(count <= 8) + 3;
            if remaining.len() < padding + count
                || remaining[..padding]
                    .iter()
                    .any(|&unit| !matches!(unit, 9 | 32))
            {
                return Err(INVALID_DATA);
            }
            remaining = &remaining[padding + count..];
        }
    }
    if out.is_empty() {
        Err(INVALID_DATA)
    } else {
        Ok(out)
    }
}
pub(super) fn decode(input: &[u16], binary: &[u8], flags: u32) -> Result<(Vec<u8>, u32, u32), u32> {
    if flags & !0x200000ff != 0 {
        return Err(INVALID_PARAMETER);
    }
    let format = flags & 0xff;
    let strict = flags & 0x20000000 != 0;
    match format {
        0 | 3 | 9 => header(input, strict).map(|(out, skip)| (out, skip, format)),
        1 => base64(input, strict).map(|out| (out, 0, 1)),
        2 => Ok((binary.to_vec(), 0, 2)),
        4 | 5 | 10 | 11 | 12 => hex(input, format).map(|out| (out, 0, format)),
        6 | 7 => header(input, strict)
            .map(|(out, skip)| (out, skip, 0))
            .or_else(|_| base64(input, strict).map(|out| (out, 0, 1)))
            .or_else(|error| {
                if format == 7 {
                    Ok((binary.to_vec(), 0, 2))
                } else {
                    Err(error)
                }
            }),
        8 => [10, 11, 4, 12, 5]
            .into_iter()
            .find_map(|format| hex(input, format).ok().map(|out| (out, 0, format)))
            .ok_or(INVALID_DATA),
        _ => Err(INVALID_PARAMETER),
    }
}
