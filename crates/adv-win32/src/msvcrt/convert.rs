//! C-locale unsigned wide conversion with x86 EDX:EAX and shared CRT errno.
use super::*;
use adv_abi::{GuestPtr, WideStr, win32};
use adv_cpu::backend::Prot;

pub(super) fn register(reg: &mut Registry) {
    reg.register(wcstoull_ABI);
    reg.register(adv_abi::Api {
        module: "api-ms-win-crt-convert-l1-1-0.dll",
        ..wcstoull_ABI
    });
}

fn digit(unit: u16) -> Option<u32> {
    match unit {
        48..=57 => Some(u32::from(unit - 48)),
        65..=90 => Some(u32::from(unit - 65 + 10)),
        97..=122 => Some(u32::from(unit - 97 + 10)),
        _ => None,
    }
}
fn parse(text: &[u16], base: i32) -> (u64, usize, Option<i32>) {
    if base != 0 && !(2..=36).contains(&base) {
        return (0, 0, Some(22));
    }
    let mut at = 0;
    while text.get(at).is_some_and(|c| matches!(c, 9..=13 | 32)) {
        at += 1;
    }
    let negative = text.get(at) == Some(&45);
    if negative || text.get(at) == Some(&43) {
        at += 1;
    }
    let mut radix = base as u32;
    if (radix == 0 || radix == 16)
        && text.get(at) == Some(&48)
        && text.get(at + 1).is_some_and(|c| *c == 120 || *c == 88)
    {
        radix = 16;
        at += 2;
    }
    if radix == 0 {
        radix = if text.get(at) == Some(&48) { 8 } else { 10 };
    }
    let begin = at;
    let mut value = 0u64;
    let mut overflow = false;
    while let Some(d) = text.get(at).and_then(|c| digit(*c)).filter(|d| *d < radix) {
        match value
            .checked_mul(u64::from(radix))
            .and_then(|v| v.checked_add(u64::from(d)))
        {
            Some(next) => value = next,
            None => overflow = true,
        }
        at += 1;
    }
    if at == begin {
        return (0, 0, None);
    }
    if overflow {
        (u64::MAX, at, Some(34))
    } else {
        (
            if negative {
                value.wrapping_neg()
            } else {
                value
            },
            at,
            None,
        )
    }
}

#[win32(ucrtbase, name="wcstoull", level=implement)]
fn wcstoull(env: &mut Env<'_>, text: WideStr, end: GuestPtr<u32>, base: i32) -> Outcome {
    if !end.is_null()
        && let Err(error) = adv_abi::guest::check_range(env.cpu, end.addr(), 4, Prot::WRITE)
    {
        return adv_abi::guest::memory_failure(error);
    }
    let value = match text.read(env.cpu, 1024 * 1024) {
        Ok(value) => value,
        Err(error) => return adv_abi::guest::memory_failure(error),
    };
    let (number, consumed, error) = parse(&value, base);
    if !end.is_null() {
        let address = text.0.offset(consumed as u32).map(GuestPtr::addr);
        if let Err(error) = address.and_then(|address| end.write(env.cpu, address)) {
            return adv_abi::guest::memory_failure(error);
        }
    }
    if let Some(error) = error {
        let pointer = floating::errno_address(env);
        if let Err(error) = pointer.write(env.cpu, error) {
            return adv_abi::guest::memory_failure(error);
        }
    }
    Outcome::Return64(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_sign_prefix_and_end_positions() {
        for (text, base, expected) in [
            ("18446744073709551615!", 10, (u64::MAX, 20, None)),
            ("18446744073709551616!", 10, (u64::MAX, 20, Some(34))),
            ("  -1x", 0, (u64::MAX, 4, None)),
            ("  +0x1fZ", 0, (31, 7, None)),
            ("0xG", 0, (0, 0, None)),
            ("09", 0, (0, 1, None)),
            (" +猫", 10, (0, 0, None)),
            ("z", 36, (35, 1, None)),
            ("12", 1, (0, 0, Some(22))),
        ] {
            assert_eq!(
                parse(&text.encode_utf16().collect::<Vec<_>>(), base),
                expected,
                "{text}"
            );
        }
    }
}
