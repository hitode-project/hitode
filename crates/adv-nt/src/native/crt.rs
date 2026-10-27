//! Locale-independent Wine 11 PE string and memory functions.

use adv_cpu::backend::{Guest, Prot};
use adv_machine::{Conv, Env, layout::PAGE};

use super::{Body, resume_original};

// Each slice stops at a guest page boundary, except an unaligned wide unit
// straddling it, whose two page spans are checked separately.
fn page(cpu: &dyn Guest, at: u32, width: u32, limit: u32) -> Option<&[u8]> {
    let rest = PAGE - at % PAGE;
    let len = (rest / width).max(1).min(limit) * width;
    at.checked_add(len - 1)?;
    if !cpu.accessible(at, len.min(rest), Prot::READ)
        || (len > rest && !cpu.accessible(at + rest, len - rest, Prot::READ))
    {
        return None;
    }
    cpu.shared_bytes(at, len as usize)
}

fn value(bytes: &[u8], width: u32) -> u32 {
    if width == 1 {
        u32::from(bytes[0])
    } else {
        u32::from(u16::from_le_bytes([bytes[0], bytes[1]]))
    }
}

pub(super) fn string(cpu: &dyn Guest, at: u32, width: u32) -> Option<&[u8]> {
    let len = scan_length(cpu, at, width, u32::MAX)?
        .checked_add(1)?
        .checked_mul(width)?;
    cpu.shared_bytes(at, len as usize)
}

fn scan_length(cpu: &dyn Guest, mut at: u32, width: u32, limit: u32) -> Option<u32> {
    let mut count = 0;
    while count < limit {
        let bytes = page(cpu, at, width, limit - count)?;
        let zero = if width == 1 {
            bytes.iter().position(|&ch| ch == 0)
        } else {
            bytes.as_chunks::<2>().0.iter().position(|ch| *ch == [0, 0])
        };
        if let Some(i) = zero {
            return count.checked_add(i as u32);
        }
        count += bytes.len() as u32 / width;
        if count < limit {
            at = at.checked_add(bytes.len() as u32)?;
        }
    }
    Some(limit)
}

// spec: NAT-BODY-6
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

fn compare(env: &Env<'_>, width: u32, mut limit: u32, terminated: bool, sign: bool) -> Option<u64> {
    let (mut a, mut b) = (env.arg(0), env.arg(1));
    let (mut x, mut y) = (&[][..], &[][..]);
    while limit != 0 {
        if x.is_empty() {
            x = page(env.cpu, a, width, limit)?;
        }
        if y.is_empty() {
            y = page(env.cpu, b, width, limit)?;
        }
        let len = y.len().min(x.len());
        if !terminated {
            let order = x[..len].cmp(&y[..len]);
            if order != std::cmp::Ordering::Equal {
                return Some(u64::from(order as i32 as u32));
            }
        } else {
            let stop = if width == 1 {
                x[..len]
                    .iter()
                    .zip(&y[..len])
                    .position(|(&x, &y)| x != y || x == 0)
            } else {
                x[..len]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .zip(y[..len].as_chunks::<2>().0)
                    .position(|(x, y)| x != y || *x == [0, 0])
            };
            if let Some(i) = stop {
                let at = i * width as usize;
                let diff = value(&x[at..], width) as i32 - value(&y[at..], width) as i32;
                return Some(u64::from((if sign { diff.signum() } else { diff }) as u32));
            }
        }
        x = &x[len..];
        y = &y[len..];
        limit -= len as u32 / width;
        if limit != 0 {
            a = a.checked_add(len as u32)?;
            b = b.checked_add(len as u32)?;
        }
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
    Some(u64::from(scan_length(env.cpu, env.arg(0), width, limit)?))
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

fn find(env: &Env<'_>, width: u32, mut limit: u32, terminated: bool, last: bool) -> Option<u64> {
    let mut at = env.arg(0);
    let ch = env.arg(1) & if width == 1 { 0xff } else { 0xffff };
    let mut found = 0;
    while limit != 0 {
        let bytes = page(env.cpu, at, width, limit)?;
        for (i, unit) in bytes.chunks_exact(width as usize).enumerate() {
            let x = value(unit, width);
            if x == ch {
                found = at + i as u32 * width;
                if !last {
                    return Some(u64::from(found));
                }
            }
            if terminated && x == 0 {
                return Some(u64::from(found));
            }
        }
        limit -= bytes.len() as u32 / width;
        if limit != 0 {
            at = at.checked_add(bytes.len() as u32)?;
        }
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
    let data = string(env.cpu, src, width)?;
    let offset = if append {
        string(env.cpu, dst, width)?.len() as u32 - width
    } else {
        0
    };
    let at = dst.checked_add(offset)?;
    let end = at.checked_add(data.len() as u32)?;
    // Wine copies forwards. Let it handle aliases whose writes change later reads.
    if at < src.checked_add(data.len() as u32)? && src < end {
        return None;
    }
    let len = data.len();
    if !env.cpu.accessible(at, len as u32, Prot::WRITE) {
        return None;
    }
    env.cpu.copy_memory(src, at, len).ok()?;
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
    if check_haystack && value(page(env.cpu, env.arg(0), width, 1)?, width) == 0 {
        return Some(0);
    }
    let needle = string(env.cpu, env.arg(1), width)?;
    let needle = &needle[..needle.len() - width as usize];
    if needle.is_empty() {
        return Some(u64::from(env.arg(0)));
    }
    let haystack = string(env.cpu, env.arg(0), width)?;
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

const ALL: &[&str] = &[
    "ntdll.dll",
    "msvcrt.dll",
    "ucrtbase.dll",
    "msvcr70.dll",
    "msvcr71.dll",
    "msvcr80.dll",
    "msvcr90.dll",
    "msvcr100.dll",
    "msvcr110.dll",
    "msvcr120.dll",
    "msvcrtd.dll",
];
const WITH_STRNLEN: &[&str] = &[
    "ntdll.dll",
    "msvcrt.dll",
    "ucrtbase.dll",
    "msvcr80.dll",
    "msvcr90.dll",
    "msvcr100.dll",
    "msvcr110.dll",
    "msvcr120.dll",
];
const DIFF_COMPARISONS: &[&str] = &[
    "ntdll.dll",
    "msvcrt.dll",
    "msvcr80.dll",
    "msvcr90.dll",
    "msvcr100.dll",
    "msvcrtd.dll",
];
const NTDLL: &[&str] = &["ntdll.dll"];
const CRT: &[&str] = &[
    "msvcrt.dll",
    "ucrtbase.dll",
    "msvcr70.dll",
    "msvcr71.dll",
    "msvcr80.dll",
    "msvcr90.dll",
    "msvcr100.dll",
    "msvcr110.dll",
    "msvcr120.dll",
    "msvcrtd.dll",
];
const SIGN_COMPARISONS: &[&str] = &[
    "ucrtbase.dll",
    "msvcr70.dll",
    "msvcr71.dll",
    "msvcr110.dll",
    "msvcr120.dll",
];

pub(super) fn bodies() -> Vec<Body> {
    type Variant = (super::BodyFn, &'static [&'static str]);
    let table: &[(&str, u8, &[Variant])] = &[
        ("memset", 3, &[(memset, ALL)]),
        ("memmove", 3, &[(memmove, CRT)]),
        ("memcmp", 3, &[(memcmp, ALL)]),
        ("memchr", 3, &[(memchr, ALL)]),
        ("strlen", 1, &[(strlen, ALL)]),
        ("strnlen", 2, &[(strnlen, WITH_STRNLEN)]),
        ("wcslen", 1, &[(wcslen, ALL)]),
        ("wcsnlen", 2, &[(wcsnlen, WITH_STRNLEN)]),
        ("strcmp", 2, &[(strcmp, ALL)]),
        (
            "strncmp",
            3,
            &[
                (strncmp, DIFF_COMPARISONS),
                (strncmp_sign, SIGN_COMPARISONS),
            ],
        ),
        ("wcscmp", 2, &[(wcscmp, NTDLL), (wcscmp_sign, CRT)]),
        ("wcsncmp", 3, &[(wcsncmp, ALL)]),
        ("strchr", 2, &[(strchr, ALL)]),
        ("strrchr", 2, &[(strrchr, ALL)]),
        ("wcschr", 2, &[(wcschr, ALL)]),
        ("wcsrchr", 2, &[(wcsrchr, ALL)]),
        ("strcpy", 2, &[(strcpy, ALL)]),
        ("wcscpy", 2, &[(wcscpy, ALL)]),
        ("strcat", 2, &[(strcat, ALL)]),
        ("wcscat", 2, &[(wcscat, ALL)]),
        ("strstr", 2, &[(strstr_nt, NTDLL), (strstr, CRT)]),
        ("wcsstr", 2, &[(wcsstr, ALL)]),
    ];
    let mut bodies = Vec::new();
    for &(name, argc, variants) in table {
        for &(body, modules) in variants {
            for module in modules {
                bodies.push(Body {
                    name: format!("{module}!{name}").into(),
                    conv: Conv::Cdecl { argc },
                    body,
                    handler: resume_original,
                });
            }
        }
    }
    bodies
}
