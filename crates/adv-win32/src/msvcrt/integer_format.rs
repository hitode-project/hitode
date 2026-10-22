//! Radix output shares a byte/UTF-16 kernel and native secure-buffer ordering.
//! Signed inputs use a sign only in base ten; other bases use the x86 bit pattern.
use super::*;
use adv_abi::{GuestPtr, guest::memory_failure};
use adv_cpu::backend::MemFault;

const NAMES: &[&str] = &[
    "_itoa",
    "_ltoa",
    "_ultoa",
    "_i64toa",
    "_ui64toa",
    "_itow",
    "_ltow",
    "_ultow",
    "_i64tow",
    "_ui64tow",
    "_itoa_s",
    "_ltoa_s",
    "_ultoa_s",
    "_i64toa_s",
    "_ui64toa_s",
    "_itow_s",
    "_ltow_s",
    "_ultow_s",
    "_i64tow_s",
    "_ui64tow_s",
];
pub(super) fn register(registry: &mut Registry) {
    let exports: Vec<_> = NAMES
        .iter()
        .map(|&n| (n, call as adv_machine::Handler))
        .collect();
    versions::register(registry, &exports);
}

#[derive(Clone, Copy)]
struct Buffer {
    pointer: u32,
    wide: bool,
}
impl Buffer {
    fn write(self, env: &mut Env<'_>, index: u32, unit: u16) -> Result<(), MemFault> {
        if self.wide {
            GuestPtr::<u16>::new(self.pointer)
                .offset(index)?
                .write(env.cpu, unit)
        } else {
            GuestPtr::<u8>::new(self.pointer)
                .offset(index)?
                .write(env.cpu, unit as u8)
        }
    }
    fn read(self, env: &Env<'_>, index: u32) -> Result<u16, MemFault> {
        if self.wide {
            GuestPtr::<u16>::new(self.pointer)
                .offset(index)?
                .read(env.cpu)
        } else {
            GuestPtr::<u8>::new(self.pointer)
                .offset(index)?
                .read(env.cpu)
                .map(u16::from)
        }
    }
}

fn output(
    env: &mut Env<'_>,
    buffer: Buffer,
    mut value: u64,
    negative: bool,
    radix: u32,
    capacity: u32,
) -> Result<bool, MemFault> {
    let begin = u32::from(negative);
    if negative {
        buffer.write(env, 0, u16::from(b'-'))?;
    }
    let mut at = begin;
    loop {
        let digit = (value % u64::from(radix)) as u8;
        buffer.write(
            env,
            at,
            u16::from(if digit < 10 {
                b'0' + digit
            } else {
                b'a' + digit - 10
            }),
        )?;
        at += 1;
        value /= u64::from(radix);
        if value == 0 || at >= capacity {
            break;
        }
    }
    if at >= capacity {
        buffer.write(env, 0, 0)?;
        return Ok(false);
    }
    buffer.write(env, at, 0)?;
    let (mut left, mut right) = (begin, at - 1);
    while left < right {
        let (a, b) = (buffer.read(env, left)?, buffer.read(env, right)?);
        buffer.write(env, left, b)?;
        buffer.write(env, right, a)?;
        left += 1;
        right -= 1;
    }
    Ok(true)
}

fn call(env: &mut Env<'_>) -> Outcome {
    let name = env.sys.thunks.get(env.thunk).name.clone();
    let full = name.contains("64");
    let secure = name.ends_with("_s");
    let index = if full { 2 } else { 1 };
    let raw = u64::from(env.arg(0)) | if full { u64::from(env.arg(1)) << 32 } else { 0 };
    let buffer = Buffer {
        pointer: env.arg(index),
        wide: name.contains('w'),
    };
    let capacity = if secure { env.arg(index + 1) } else { u32::MAX };
    let radix = env.arg(index + if secure { 2 } else { 1 });
    let signed = !name.starts_with("_u");
    let negative = signed && radix == 10 && raw & (1 << if full { 63 } else { 31 }) != 0;
    let value = if negative {
        if full {
            raw.wrapping_neg()
        } else {
            u64::from((raw as u32).wrapping_neg())
        }
    } else {
        raw
    };
    if secure {
        if buffer.pointer == 0 || capacity == 0 {
            return errors::validation(env, 22, Outcome::Return(22));
        }
        if let Err(fault) = buffer.write(env, 0, 0) {
            return memory_failure(fault);
        }
        // Native VC90 checks minimum capacity before checking the radix.
        if capacity <= 1 + u32::from(negative) {
            return errors::validation(env, 34, Outcome::Return(34));
        }
    }
    if !(2..=36).contains(&radix) {
        return errors::validation(env, 22, Outcome::Return(if secure { 22 } else { 0 }));
    }
    match output(env, buffer, value, negative, radix, capacity) {
        Ok(true) => Outcome::Return(if secure { 0 } else { buffer.pointer }),
        Ok(false) => errors::validation(env, 34, Outcome::Return(34)),
        Err(fault) => memory_failure(fault),
    }
}
