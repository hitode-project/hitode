//! Locale-independent Wine 11 PE string and memory functions.

use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::Env;

fn unit(cpu: &dyn Guest, at: u32, width: u32) -> Option<u32> {
    if !cpu.accessible(at, width, Prot::READ) {
        return None;
    }
    if width == 1 {
        Some(u32::from(cpu.read_u8(at).ok()?))
    } else {
        Some(u32::from(cpu.read_u16(at).ok()?))
    }
}

// Stop at the terminator, including when it touches the final mapped page.
fn string(cpu: &dyn Guest, at: u32, width: u32, limit: u32) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    let mut pos = at;
    for _ in 0..limit {
        let ch = unit(cpu, pos, width)?;
        data.extend_from_slice(&ch.to_le_bytes()[..width as usize]);
        if ch == 0 {
            break;
        }
        pos = pos.checked_add(width)?;
    }
    Some(data)
}

fn write(cpu: &mut dyn Guest, at: u32, data: &[u8]) -> Option<()> {
    if !data.is_empty() {
        if !cpu.accessible(at, data.len() as u32, Prot::WRITE) {
            return None;
        }
        cpu.write(at, data).ok()?;
    }
    Some(())
}

// spec: NAT-BODY-5
pub(super) fn memmove(env: &mut Env<'_>) -> Option<u64> {
    let (dst, src, len) = (env.arg(0), env.arg(1), env.arg(2));
    if len != 0 {
        if !env.cpu.accessible(src, len, Prot::READ) || !env.cpu.accessible(dst, len, Prot::WRITE) {
            return None;
        }
        env.cpu.copy_memory(src, dst, len as usize).ok()?;
    }
    Some(u64::from(dst) << 32 | u64::from(dst))
}

pub(super) fn memset(env: &mut Env<'_>) -> Option<u64> {
    let (dst, ch, len) = (env.arg(0), env.arg(1), env.arg(2));
    if len != 0 {
        env.cpu.fill_memory(dst, ch as u8, len as usize).ok()?;
    }
    Some(u64::from(dst))
}

fn compare(env: &Env<'_>, width: u32, limit: u32, terminated: bool, sign: bool) -> Option<u64> {
    let (mut a, mut b) = (env.arg(0), env.arg(1));
    for _ in 0..limit {
        let (x, y) = (unit(env.cpu, a, width)?, unit(env.cpu, b, width)?);
        if x != y {
            let diff = x as i32 - y as i32;
            return Some(u64::from((if sign { diff.signum() } else { diff }) as u32));
        }
        if terminated && x == 0 {
            break;
        }
        a = a.checked_add(width)?;
        b = b.checked_add(width)?;
    }
    Some(0)
}

pub(super) fn memcmp(env: &mut Env<'_>) -> Option<u64> {
    compare(env, 1, env.arg(2), false, true)
}

pub(super) fn strcmp(env: &mut Env<'_>) -> Option<u64> {
    compare(env, 1, u32::MAX, true, true)
}

pub(super) fn strncmp(env: &mut Env<'_>) -> Option<u64> {
    compare(env, 1, env.arg(2), true, false)
}

pub(super) fn strncmp_sign(env: &mut Env<'_>) -> Option<u64> {
    compare(env, 1, env.arg(2), true, true)
}

pub(super) fn wcscmp(env: &mut Env<'_>) -> Option<u64> {
    compare(env, 2, u32::MAX, true, false)
}

pub(super) fn wcscmp_sign(env: &mut Env<'_>) -> Option<u64> {
    compare(env, 2, u32::MAX, true, true)
}

pub(super) fn wcsncmp(env: &mut Env<'_>) -> Option<u64> {
    compare(env, 2, env.arg(2), true, false)
}

fn length(env: &Env<'_>, width: u32, limit: u32) -> Option<u64> {
    let mut at = env.arg(0);
    for count in 0..limit {
        if unit(env.cpu, at, width)? == 0 {
            return Some(u64::from(count));
        }
        at = at.checked_add(width)?;
    }
    Some(u64::from(limit))
}

pub(super) fn strlen(env: &mut Env<'_>) -> Option<u64> {
    length(env, 1, u32::MAX)
}
pub(super) fn strnlen(env: &mut Env<'_>) -> Option<u64> {
    length(env, 1, env.arg(1))
}
pub(super) fn wcslen(env: &mut Env<'_>) -> Option<u64> {
    length(env, 2, u32::MAX)
}
pub(super) fn wcsnlen(env: &mut Env<'_>) -> Option<u64> {
    length(env, 2, env.arg(1))
}

fn find(env: &Env<'_>, width: u32, limit: u32, terminated: bool, last: bool) -> Option<u64> {
    let mut at = env.arg(0);
    let ch = env.arg(1) & if width == 1 { 0xff } else { 0xffff };
    let mut found = 0;
    for _ in 0..limit {
        let x = unit(env.cpu, at, width)?;
        if x == ch {
            found = at;
            if !last {
                break;
            }
        }
        if terminated && x == 0 {
            break;
        }
        at = at.checked_add(width)?;
    }
    Some(u64::from(found))
}

pub(super) fn memchr(env: &mut Env<'_>) -> Option<u64> {
    find(env, 1, env.arg(2), false, false)
}
pub(super) fn strchr(env: &mut Env<'_>) -> Option<u64> {
    find(env, 1, u32::MAX, true, false)
}
pub(super) fn strrchr(env: &mut Env<'_>) -> Option<u64> {
    find(env, 1, u32::MAX, true, true)
}
pub(super) fn wcschr(env: &mut Env<'_>) -> Option<u64> {
    find(env, 2, u32::MAX, true, false)
}
pub(super) fn wcsrchr(env: &mut Env<'_>) -> Option<u64> {
    find(env, 2, u32::MAX, true, true)
}

fn copy(env: &mut Env<'_>, width: u32, append: bool) -> Option<u64> {
    let (dst, src) = (env.arg(0), env.arg(1));
    let data = string(env.cpu, src, width, u32::MAX)?;
    let offset = if append {
        string(env.cpu, dst, width, u32::MAX)?.len() as u32 - width
    } else {
        0
    };
    let at = dst.checked_add(offset)?;
    let end = at.checked_add(data.len() as u32)?;
    // Wine copies forwards. Let it handle aliases whose writes change later reads.
    if at < src.checked_add(data.len() as u32)? && src < end {
        return None;
    }
    write(env.cpu, at, &data)?;
    Some(u64::from(dst))
}

pub(super) fn strcpy(env: &mut Env<'_>) -> Option<u64> {
    copy(env, 1, false)
}
pub(super) fn wcscpy(env: &mut Env<'_>) -> Option<u64> {
    copy(env, 2, false)
}
pub(super) fn strcat(env: &mut Env<'_>) -> Option<u64> {
    copy(env, 1, true)
}
pub(super) fn wcscat(env: &mut Env<'_>) -> Option<u64> {
    copy(env, 2, true)
}

fn substring(env: &Env<'_>, width: u32, check_haystack: bool) -> Option<u64> {
    if check_haystack && unit(env.cpu, env.arg(0), width)? == 0 {
        return Some(0);
    }
    let needle = string(env.cpu, env.arg(1), width, u32::MAX)?;
    let needle = &needle[..needle.len() - width as usize];
    if needle.is_empty() {
        return Some(u64::from(env.arg(0)));
    }
    let haystack = string(env.cpu, env.arg(0), width, u32::MAX)?;
    let offset = haystack
        .windows(needle.len())
        .enumerate()
        .step_by(width as usize)
        .find_map(|(i, window)| (window == needle).then_some(i));
    Some(offset.map_or(0, |i| u64::from(env.arg(0)) + i as u64))
}

pub(super) fn strstr(env: &mut Env<'_>) -> Option<u64> {
    substring(env, 1, false)
}
pub(super) fn wcsstr(env: &mut Env<'_>) -> Option<u64> {
    substring(env, 2, true)
}

pub(super) fn strstr_nt(env: &mut Env<'_>) -> Option<u64> {
    substring(env, 1, true)
}
