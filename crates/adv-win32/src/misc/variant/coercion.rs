//! Basic Automation coercion over the existing VARIANT and BSTR owners.
//! Native evidence: variant-coercion-native/probe.c, SHA256
//! 242D5EF6952F3FDA9A9014801B9D996115F9C1A6BFB8297BBD1AD7F64F0EB987.
use super::*;

const API: &str = "oleaut32.dll!VariantChangeType";
const MISMATCH: u32 = 0x80020005;

pub(super) fn register(registry: &mut Registry) {
    registry.register(change_ABI);
    // The native named export and ordinal 12 have the same address.
    registry.register(Api {
        name: "#12",
        ..change_ABI
    });
}

fn unavailable(env: &mut Env<'_>, source: u16, target: u16, flags: u16, reason: &str) -> u32 {
    warn(
        env,
        API,
        &format!("{reason}; sourceVT=0x{source:04x}, targetVT=0x{target:04x}, flags=0x{flags:04x}"),
        NOT_IMPLEMENTED,
    )
}

pub(super) fn text(env: &Env<'_>, value: u32) -> Result<String, u32> {
    if value == 0 {
        return Ok(String::new());
    }
    if !owns_bstr(env, value) {
        return Err(INVALID);
    }
    let prefix = value.checked_sub(4).ok_or(INVALID)?;
    check_range(env.cpu, prefix, 4, Prot::READ).map_err(|_| INVALID)?;
    let length = env.cpu.read_u32(prefix).map_err(|_| INVALID)?;
    if length > MAX_BSTR_BYTES || length % 2 != 0 {
        return Err(INVALID);
    }
    check_range(env.cpu, value, length, Prot::READ).map_err(|_| INVALID)?;
    let mut bytes = vec![0; length as usize];
    env.cpu.read(value, &mut bytes).map_err(|_| INVALID)?;
    let units: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    // Embedded-NUL/localized spelling is a separate conversion profile.
    String::from_utf16(&units).map_err(|_| INVALID)
}

fn numeric_text(text: &str) -> bool {
    let mut bytes = text.as_bytes();
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        bytes = &bytes[1..];
    }
    let mut digits = 0;
    while bytes.first().is_some_and(u8::is_ascii_digit) {
        digits += 1;
        bytes = &bytes[1..];
    }
    if bytes.first() == Some(&b'.') {
        bytes = &bytes[1..];
        while bytes.first().is_some_and(u8::is_ascii_digit) {
            digits += 1;
            bytes = &bytes[1..];
        }
    }
    if digits == 0 {
        return false;
    }
    if matches!(bytes.first(), Some(b'e' | b'E')) {
        bytes = &bytes[1..];
        if matches!(bytes.first(), Some(b'+' | b'-')) {
            bytes = &bytes[1..];
        }
        if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
            return false;
        }
        bytes = &[];
    }
    bytes.is_empty()
}

fn boolean(env: &mut Env<'_>, bytes: &Variant, flags: u16, target: u16) -> Result<i16, u32> {
    let source = vt(bytes);
    let unsupported = |env: &mut Env<'_>, reason| unavailable(env, source, target, flags, reason);
    if flags != 0 || target != 11 {
        return Err(unsupported(
            env,
            "only flags-zero conversion to VT_BOOL is implemented",
        ));
    }
    let value = match source {
        0 => false,
        1 => return Err(MISMATCH),
        2 => i16::from_le_bytes(bytes[8..10].try_into().unwrap()) != 0,
        3 | 19 | 22 | 23 => pointer(bytes) != 0,
        16 | 17 => bytes[8] != 0,
        18 => u16::from_le_bytes(bytes[8..10].try_into().unwrap()) != 0,
        20 | 21 => u64::from_le_bytes(bytes[8..16].try_into().unwrap()) != 0,
        11 => return Ok(i16::from_le_bytes(bytes[8..10].try_into().unwrap())),
        4 | 5 => {
            let value = if source == 4 {
                f64::from(f32::from_le_bytes(bytes[8..12].try_into().unwrap()))
            } else {
                f64::from_le_bytes(bytes[8..16].try_into().unwrap())
            };
            if !value.is_finite() {
                return Err(unsupported(
                    env,
                    "nonfinite numeric Automation coercion is unavailable",
                ));
            }
            value != 0.
        }
        8 => {
            let text = text(env, pointer(bytes)).map_err(|error| {
                if error == INVALID {
                    warn(
                        env,
                        API,
                        "source BSTR is not an owned, readable UTF-16 Automation value",
                        error,
                    )
                } else {
                    error
                }
            })?;
            if !text.is_ascii() || text.contains('\0') || text.contains(',') {
                return Err(unsupported(
                    env,
                    "localized/grouped/embedded-NUL BSTR coercion is unavailable",
                ));
            }
            if text.eq_ignore_ascii_case("true") {
                true
            } else if text.eq_ignore_ascii_case("false") {
                false
            } else {
                let text = text.trim_matches(|c: char| c.is_ascii_whitespace());
                if !numeric_text(text) {
                    return Err(MISMATCH);
                }
                let value = text.parse::<f64>().map_err(|_| MISMATCH)?;
                if !value.is_finite() {
                    return Err(unsupported(
                        env,
                        "out-of-range numeric BSTR coercion is unavailable",
                    ));
                }
                let nonzero_mantissa = text
                    .split(['e', 'E'])
                    .next()
                    .unwrap()
                    .bytes()
                    .any(|b| matches!(b, b'1'..=b'9'));
                if value.is_subnormal() || (value == 0. && nonzero_mantissa) {
                    return Err(unsupported(
                        env,
                        "subnormal/underflow numeric BSTR coercion is outside the verified profile",
                    ));
                }
                value != 0.
            }
        }
        _ => {
            return Err(unsupported(
                env,
                "source type requires unsupported Automation coercion",
            ));
        }
    };
    Ok(if value { -1 } else { 0 })
}

#[win32(oleaut32, name = "VariantChangeType", level = implement)]
fn change(
    env: &mut Env<'_>,
    destination: GuestPtr<Variant>,
    source: GuestPtr<Variant>,
    flags: u32,
    target: u32,
) -> Outcome {
    let bytes = match read(env, source.addr(), false) {
        Ok(bytes) => bytes,
        Err(error) => return Outcome::Return(error),
    };
    if let Err(error) = supported(env, vt(&bytes), API) {
        return Outcome::Return(error);
    }
    if let Err(error) = supported(env, target as u16, API) {
        return Outcome::Return(error);
    }
    if read(env, destination.addr(), true).is_err() {
        return Outcome::Return(INVALID);
    }
    // Compute before releasing destination ownership. Native failures preserve
    // its entire value, including an in-place source BSTR.
    let value = match boolean(env, &bytes, flags as u16, target as u16) {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    clear_then(
        env,
        destination.addr(),
        API,
        Box::new(move |env| {
            let mut bytes = match read(env, destination.addr(), true) {
                Ok(bytes) => bytes,
                Err(error) => return Outcome::Return(error),
            };
            // Reserved/unused bytes have no coercion result meaning. Preserve the
            // post-clear bytes; only vt and the VARIANT_BOOL payload are published.
            bytes[..2].copy_from_slice(&11u16.to_le_bytes());
            bytes[8..10].copy_from_slice(&value.to_le_bytes());
            match env.cpu.write(destination.addr(), &bytes) {
                Ok(()) => Outcome::Return(0),
                Err(_) => Outcome::Return(INVALID),
            }
        }),
    )
}
