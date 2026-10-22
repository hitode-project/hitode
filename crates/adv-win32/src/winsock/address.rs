//! Address conversion is local arithmetic and never contacts a host resolver.
use crate::util::set_last_error;
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;
use std::net::{Ipv4Addr, Ipv6Addr};

pub(super) fn register(registry: &mut Registry) {
    for api in [
        htonl_ABI, htons_ABI, ntohl_ABI, ntohs_ABI, addr_ABI, pton_a_ABI, pton_w_ABI, ntop_a_ABI,
        ntop_w_ABI,
    ] {
        registry.register(api);
    }
}

#[win32(ws2_32, name="htonl", level=implement)]
fn htonl(_env: &mut Env<'_>, value: u32) -> u32 {
    value.swap_bytes()
}
#[win32(ws2_32, name="ntohl", level=implement)]
fn ntohl(_env: &mut Env<'_>, value: u32) -> u32 {
    value.swap_bytes()
}
#[win32(ws2_32, name="htons", level=implement)]
fn htons(_env: &mut Env<'_>, value: u32) -> u32 {
    u32::from((value as u16).swap_bytes())
}
#[win32(ws2_32, name="ntohs", level=implement)]
fn ntohs(_env: &mut Env<'_>, value: u32) -> u32 {
    u32::from((value as u16).swap_bytes())
}

fn legacy_address(text: &[u8]) -> Option<[u8; 4]> {
    // winsock.h's historical one-, two-, three- and four-part notation.
    // A leading zero denotes octal; 0x denotes hexadecimal. The last part
    // occupies all remaining bytes, with overflow rejected before narrowing.
    let end = text
        .iter()
        .position(u8::is_ascii_whitespace)
        .unwrap_or(text.len());
    if text[end..].iter().any(|byte| !byte.is_ascii_whitespace()) {
        return None;
    }
    let parts: Vec<_> = text[..end].split(|byte| *byte == b'.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut values = Vec::new();
    for part in &parts {
        let (base, digits) = if part.starts_with(b"0x") || part.starts_with(b"0X") {
            (16u32, &part[2..])
        } else if part.len() > 1 && part[0] == b'0' {
            (8, &part[1..])
        } else {
            (10, *part)
        };
        if digits.is_empty() {
            return None;
        }
        let mut value = 0u32;
        for &digit in digits {
            let digit = match digit {
                b'0'..=b'9' => u32::from(digit - b'0'),
                b'a'..=b'f' => u32::from(digit - b'a') + 10,
                b'A'..=b'F' => u32::from(digit - b'A') + 10,
                _ => return None,
            };
            if digit >= base {
                return None;
            }
            value = value.checked_mul(base)?.checked_add(digit)?;
        }
        values.push(value);
    }
    let mut address = 0u32;
    for (index, value) in values.iter().copied().enumerate() {
        if index + 1 == parts.len() {
            let bits = 8 * (5 - parts.len());
            if bits < 32 && value >= (1 << bits) {
                return None;
            }
            address |= value;
        } else {
            if value > 255 {
                return None;
            }
            address |= value << (24 - index * 8);
        }
    }
    Some(address.to_be_bytes())
}

#[win32(ws2_32, name="inet_addr", level=implement)]
fn addr(env: &mut Env<'_>, text: AnsiStr) -> u32 {
    text.read(env.cpu, 32768)
        .ok()
        .and_then(|text| legacy_address(&text))
        .map_or(u32::MAX, u32::from_le_bytes)
}

fn failure(env: &mut Env<'_>, error: u32) -> i32 {
    set_last_error(env, error);
    -1
}
fn pton(env: &mut Env<'_>, family: i32, text: u32, output: u32, wide: bool) -> i32 {
    if !matches!(family, 2 | 23) {
        return failure(env, 10047);
    }
    if text == 0 || output == 0 {
        return failure(env, 10014);
    }
    let units = match crate::network::text(env, text, wide) {
        Ok(value) => value,
        Err(_) => return failure(env, 10014),
    };
    let Ok(text) = String::from_utf16(&units) else {
        return 0;
    };
    let bytes = if family == 2 {
        text.parse::<Ipv4Addr>()
            .ok()
            .map(|address| address.octets().to_vec())
    } else {
        text.parse::<Ipv6Addr>()
            .ok()
            .map(|address| address.octets().to_vec())
    };
    let Some(bytes) = bytes else { return 0 };
    if check_range(env.cpu, output, bytes.len() as u32, Prot::WRITE).is_err()
        || env.cpu.write(output, &bytes).is_err()
    {
        return failure(env, 10014);
    }
    1
}
#[win32(ws2_32, name="inet_pton", level=implement)]
fn pton_a(env: &mut Env<'_>, family: i32, text: AnsiStr, output: GuestPtr<()>) -> i32 {
    pton(env, family, text.0.addr(), output.addr(), false)
}
#[win32(ws2_32, name="InetPtonW", level=implement)]
fn pton_w(env: &mut Env<'_>, family: i32, text: WideStr, output: GuestPtr<()>) -> i32 {
    pton(env, family, text.0.addr(), output.addr(), true)
}

fn ntop(
    env: &mut Env<'_>,
    family: i32,
    address: u32,
    output: u32,
    capacity: u32,
    wide: bool,
) -> u32 {
    let length = match family {
        2 => 4,
        23 => 16,
        _ => {
            failure(env, 10047);
            return 0;
        }
    };
    if check_range(env.cpu, address, length, Prot::READ).is_err() {
        failure(env, 10014);
        return 0;
    }
    let mut bytes = [0; 16];
    if env
        .cpu
        .read(address, &mut bytes[..length as usize])
        .is_err()
    {
        failure(env, 10014);
        return 0;
    }
    let mut text = if family == 2 {
        Ipv4Addr::from(<[u8; 4]>::try_from(&bytes[..4]).unwrap()).to_string()
    } else {
        Ipv6Addr::from(bytes).to_string()
    };
    text.push('\0');
    if capacity < text.len() as u32 {
        failure(env, 10014);
        return 0;
    }
    let bytes: Vec<_> = if wide {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    } else {
        text.into_bytes()
    };
    if check_range(env.cpu, output, bytes.len() as u32, Prot::WRITE).is_err()
        || env.cpu.write(output, &bytes).is_err()
    {
        failure(env, 10014);
        return 0;
    }
    output
}
#[win32(ws2_32, name="inet_ntop", level=implement)]
fn ntop_a(
    env: &mut Env<'_>,
    family: i32,
    address: GuestPtr<()>,
    output: AnsiStr,
    capacity: u32,
) -> AnsiStr {
    AnsiStr(GuestPtr::new(ntop(
        env,
        family,
        address.addr(),
        output.0.addr(),
        capacity,
        false,
    )))
}
#[win32(ws2_32, name="InetNtopW", level=implement)]
fn ntop_w(
    env: &mut Env<'_>,
    family: i32,
    address: GuestPtr<()>,
    output: WideStr,
    capacity: u32,
) -> WideStr {
    WideStr(GuestPtr::new(ntop(
        env,
        family,
        address.addr(),
        output.0.addr(),
        capacity,
        true,
    )))
}

#[cfg(test)]
mod tests;
