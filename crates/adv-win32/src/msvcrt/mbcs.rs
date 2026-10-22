//! Multibyte traversal uses the existing CRT locale owner and guest code pages.
//! Bytes that look like lead bytes in trail positions remain part of one pair.
use super::*;
use adv_abi::{GuestPtr, guest::memory_failure};
use adv_cpu::backend::{MemFault, Prot};

const NAMES: &[&str] = &[
    "_setmbcp",
    "_getmbcp",
    "_ismbblead",
    "_ismbbtrail",
    "_ismbslead",
    "_ismbstrail",
    "_mbsinc",
    "_mbsdec",
    "_mbsnextc",
    "_mbslen",
    "_mbsnbcnt",
    "_mbsnccnt",
    "_mbsninc",
    "_mbsnbcpy",
];

pub(super) fn register(registry: &mut Registry) {
    let exports: Vec<_> = NAMES
        .iter()
        .map(|&n| (n, call as adv_machine::Handler))
        .collect();
    versions::register(registry, &exports);
}

pub(super) fn is_lead(codepage: u32, byte: u8) -> bool {
    adv_os::nls::lead_byte_ranges(codepage)
        .iter()
        .any(|&(low, high)| (low..=high).contains(&byte))
}
fn is_trail(codepage: u32, byte: u8) -> bool {
    match codepage {
        932 => matches!(byte, 0x40..=0x7e | 0x80..=0xfc),
        936 => matches!(byte, 0x40..=0x7e | 0x80..=0xfe),
        _ => false,
    }
}
fn byte(env: &Env<'_>, address: u32) -> Result<u8, MemFault> {
    GuestPtr::new(address).read(env.cpu)
}
fn advance(address: u32, count: u32) -> Result<u32, MemFault> {
    address.checked_add(count).ok_or(MemFault {
        addr: address,
        len: count + 1,
    })
}

// Traversal accepts nonzero trail bytes without validating their character
// mapping. A terminal lead contributes neither a character nor counted bytes.
fn counts(
    env: &Env<'_>,
    address: u32,
    cp: u32,
    characters: u32,
    bytes: u32,
) -> Result<(u32, u32), MemFault> {
    let (mut used, mut count) = (0, 0);
    while used < bytes && count < characters {
        let p = advance(address, used)?;
        let b = byte(env, p)?;
        if b == 0 {
            break;
        }
        let width = if is_lead(cp, b) {
            if bytes - used < 2 || byte(env, advance(p, 1)?)? == 0 {
                break;
            }
            2
        } else {
            1
        };
        used += width;
        count += 1;
    }
    Ok((used, count))
}

fn contextual(
    env: &Env<'_>,
    mut start: u32,
    current: u32,
    cp: u32,
    trail: bool,
) -> Result<u32, MemFault> {
    if adv_os::nls::lead_byte_ranges(cp).is_empty() {
        return Ok(0);
    }
    while start <= current {
        let b = byte(env, start)?;
        if b == 0 {
            break;
        }
        let lead = is_lead(cp, b);
        if start == current {
            return Ok(if lead && !trail { u32::MAX } else { 0 });
        }
        if lead {
            start = advance(start, 1)?;
            if start == current {
                return Ok(if trail { u32::MAX } else { 0 });
            }
            if byte(env, start)? == 0 {
                break;
            }
        }
        start = advance(start, 1)?;
    }
    Ok(0)
}

fn copy(
    env: &mut Env<'_>,
    destination: u32,
    source: u32,
    count: u32,
    cp: u32,
) -> Result<u32, MemFault> {
    adv_abi::guest::check_range(env.cpu, destination, count, Prot::WRITE)?;
    let mut used = 0;
    while used < count {
        let p = advance(source, used)?;
        let first = byte(env, p)?;
        let mut width = 1;
        if first == 0
            || is_lead(cp, first) && (count - used == 1 || byte(env, advance(p, 1)?)? == 0)
        {
            memory_ops::fill(env, advance(destination, used)?, 0, count - used)?;
            break;
        }
        GuestPtr::new(advance(destination, used)?).write(env.cpu, first)?;
        if is_lead(cp, first) {
            let second = byte(env, advance(p, 1)?)?;
            GuestPtr::new(advance(destination, used + 1)?).write(env.cpu, second)?;
            width = 2;
        }
        used += width;
    }
    Ok(destination)
}

fn set_codepage(env: &mut Env<'_>) -> Outcome {
    let requested = env.arg(0) as i32;
    let win32 = env.ext.get::<crate::Win32>();
    let cp = match requested {
        -2 => win32.codec.resolve(adv_os::nls::CP_OEMCP),
        -3 => win32.codec.acp,
        -4 => locale::formatting_codepage(env),
        value if value >= 0 => value as u32,
        _ => return Outcome::Return(u32::MAX),
    };
    let profile = versions::profile(&env.sys.thunks.get(env.thunk).module);
    if cp != 0 && !adv_os::nls::is_valid_codepage(cp) || cp == 65001 && !profile.multibyte_utf8 {
        return Outcome::Return(u32::MAX);
    }
    locale::set_multibyte_codepage(env, cp);
    Outcome::Return(0)
}

fn call(env: &mut Env<'_>) -> Outcome {
    let name = env.sys.thunks.get(env.thunk).name.clone();
    let cp = locale::multibyte_codepage(env);
    let (a, b, c) = (env.arg(0), env.arg(1), env.arg(2));
    match name.as_str() {
        "_setmbcp" => return set_codepage(env),
        "_getmbcp" => return Outcome::Return(if is_lead(cp, 0x81) { cp } else { 0 }),
        "_ismbblead" => return Outcome::Return(u32::from(a <= 255 && is_lead(cp, a as u8))),
        "_ismbbtrail" => return Outcome::Return(u32::from(a <= 255 && is_trail(cp, a as u8))),
        "_mbsninc" if a == 0 => return Outcome::Return(0),
        "_mbsnbcpy" if c == 0 => return Outcome::Return(a),
        "_mbsnbcnt" | "_mbsnccnt" if b == 0 => return Outcome::Return(0),
        "_mbsinc" | "_mbsnextc" if a == 0 => return errors::invalid(env, 0),
        "_mbsdec" | "_ismbslead" | "_ismbstrail" if a == 0 || b == 0 => {
            return errors::invalid(env, 0);
        }
        "_mbsnbcpy" if a == 0 || b == 0 => return errors::invalid(env, 0),
        "_mbsnbcnt" | "_mbsnccnt" if a == 0 => return errors::invalid(env, 0),
        _ => {}
    }
    let result = (|| match name.as_str() {
        "_mbslen" => counts(env, a, cp, u32::MAX, u32::MAX).map(|(_, n)| n),
        "_mbsnbcnt" => counts(env, a, cp, b, u32::MAX).map(|(n, _)| n),
        "_mbsnccnt" => counts(env, a, cp, u32::MAX, b).map(|(_, n)| n),
        "_mbsninc" => counts(env, a, cp, b, u32::MAX).and_then(|(n, _)| advance(a, n)),
        "_mbsinc" | "_mbsnextc" => {
            let first = byte(env, a)?;
            let second = if is_lead(cp, first) {
                byte(env, advance(a, 1)?)?
            } else {
                0
            };
            if name == "_mbsinc" {
                advance(a, if second != 0 { 2 } else { 1 })
            } else {
                Ok(if second != 0 {
                    (u32::from(first) << 8) | u32::from(second)
                } else {
                    u32::from(first)
                })
            }
        }
        "_mbsdec" => {
            if a >= b {
                return Ok(0);
            }
            if adv_os::nls::lead_byte_ranges(cp).is_empty() {
                return Ok(b - 1);
            }
            let mut p = a;
            loop {
                let next = advance(p, if is_lead(cp, byte(env, p)?) { 2 } else { 1 })?;
                if next >= b {
                    return Ok(p);
                }
                p = next;
            }
        }
        "_ismbslead" | "_ismbstrail" => contextual(env, a, b, cp, name == "_ismbstrail"),
        "_mbsnbcpy" => copy(env, a, b, c, cp),
        _ => unreachable!("registered multibyte export"),
    })();
    match result {
        Ok(value) => Outcome::Return(value),
        Err(fault) => memory_failure(fault),
    }
}
