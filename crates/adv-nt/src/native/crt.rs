//! Locale-independent Wine 11 PE string and memory functions.

use adv_cpu::backend::{Guest, Prot};
use adv_cpu::body::{EAX, EDX, Input, Mem, Ret, Stop};
use adv_machine::{Conv, layout::PAGE};

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

const ARGS: &[Input] = &[Input::Stack(0), Input::Stack(1), Input::Stack(2)];

// memmove(dst, src, n), and Wine's memcpy, which jumps to it.
adv_cpu::body!(pub(super) MEMMOVE = move_bytes, inputs: ARGS, outputs: EAX | EDX, pop: 0);
adv_cpu::body!(MEMSET = fill, inputs: ARGS, outputs: EAX, pop: 0);
adv_cpu::body!(MEMCMP = compare_bytes, inputs: ARGS, outputs: EAX, pop: 0);

/// EAX and EDX are `dst`, as Wine's code leaves them.
fn move_bytes<M: Mem>(mem: &mut M, [dst, src, len, ..]: [u32; 7]) -> Result<Ret, Stop> {
    if len != 0 {
        let from = mem.input(src, len)?;
        let to = mem.output(dst, len)?;
        // SAFETY: both ranges are checked; `copy` orders overlapping ones.
        unsafe { std::ptr::copy(from, to, len as usize) };
        mem.commit()?;
    }
    Ok(Ret::from(u64::from(dst) << 32 | u64::from(dst)))
}

fn fill<M: Mem>(mem: &mut M, [dst, ch, len, ..]: [u32; 7]) -> Result<Ret, Stop> {
    if len != 0 {
        let to = mem.output(dst, len)?;
        // SAFETY: the range is checked.
        unsafe { std::ptr::write_bytes(to, ch as u8, len as usize) };
        mem.commit()?;
    }
    Ok(Ret::from(dst))
}

/// The sign of the first difference: bytes past it are never read, so a
/// page beyond it may be inaccessible.
fn compare_bytes<M: Mem>(mem: &mut M, [mut a, mut b, mut len, ..]: [u32; 7]) -> Result<Ret, Stop> {
    while len != 0 {
        let to_page = |at: u32| PAGE - at % PAGE;
        let chunk = len.min(to_page(a)).min(to_page(b));
        let x = mem.input(a, chunk)?;
        let y = mem.input(b, chunk)?;
        // SAFETY: both chunks are checked.
        let (x, y) = unsafe {
            (
                std::slice::from_raw_parts(x, chunk as usize),
                std::slice::from_raw_parts(y, chunk as usize),
            )
        };
        let order = x.cmp(y);
        if order.is_ne() {
            return Ok(Ret::from(order as i32 as u32));
        }
        (a, b, len) = (a.wrapping_add(chunk), b.wrapping_add(chunk), len - chunk);
    }
    Ok(Ret::from(0u32))
}

// A checked page span, including the complete unaligned wide unit when needed.
fn span<M: Mem>(mem: &mut M, at: u32, width: u32, limit: u32) -> Result<(*const u8, u32), Stop> {
    let count = ((PAGE - at % PAGE) / width).max(1).min(limit);
    Ok((mem.input(at, count * width)?, count))
}

// SAFETY: callers hold a checked span containing this unit.
unsafe fn unit(pointer: *const u8, width: u32) -> u32 {
    if width == 1 {
        u32::from(unsafe { pointer.read() })
    } else {
        u32::from(u16::from_le(unsafe {
            pointer.cast::<u16>().read_unaligned()
        }))
    }
}

pub(super) fn compare_strings<
    M: Mem,
    const WIDTH: u32,
    const BOUNDED: bool,
    const SIGN: bool,
    const SIMD: bool,
>(
    mem: &mut M,
    [mut a, mut b, n, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let mut left = if BOUNDED { n } else { u32::MAX };
    while left != 0 {
        let count = left
            .min(((PAGE - a % PAGE) / WIDTH).max(1))
            .min(((PAGE - b % PAGE) / WIDTH).max(1));
        let x = mem.input(a, count * WIDTH)?;
        let y = mem.input(b, count * WIDTH)?;
        let mut i = 0;
        #[cfg(target_arch = "aarch64")]
        if SIMD {
            use std::arch::aarch64::*;
            // SAFETY: a vector stays inside both checked spans. The scalar
            // loop locates the first zero or unequal unit in a stopping vector.
            unsafe {
                while i + 16 / WIDTH <= count {
                    let xp = x.add((i * WIDTH) as usize);
                    let yp = y.add((i * WIDTH) as usize);
                    let done = if WIDTH == 1 {
                        let xv = vld1q_u8(xp);
                        let yv = vld1q_u8(yp);
                        vminvq_u8(vandq_u8(vceqq_u8(xv, yv), vmvnq_u8(vceqzq_u8(xv)))) != 255
                    } else {
                        let xv = vld1q_u16(xp.cast());
                        let yv = vld1q_u16(yp.cast());
                        vminvq_u16(vandq_u16(vceqq_u16(xv, yv), vmvnq_u16(vceqzq_u16(xv)))) != 65535
                    };
                    if done {
                        break;
                    }
                    i += 16 / WIDTH;
                }
            }
        }
        while i < count {
            // SAFETY: i indexes a unit in each checked span.
            let (x, y) = unsafe {
                (
                    unit(x.add((i * WIDTH) as usize), WIDTH),
                    unit(y.add((i * WIDTH) as usize), WIDTH),
                )
            };
            if x != y || x == 0 {
                let difference = x as i32 - y as i32;
                return Ok(Ret::from(
                    (if SIGN {
                        difference.signum()
                    } else {
                        difference
                    }) as u32,
                ));
            }
            i += 1;
        }
        left -= count;
        a = a.wrapping_add(count * WIDTH);
        b = b.wrapping_add(count * WIDTH);
    }
    Ok(0u32.into())
}

fn length<M: Mem, const WIDTH: u32, const BOUNDED: bool>(
    mem: &mut M,
    [at, n, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    Ok(mem
        .terminated(at, WIDTH, if BOUNDED { n } else { u32::MAX })?
        .into())
}

fn find<M: Mem, const WIDTH: u32, const TERMINATED: bool, const LAST: bool>(
    mem: &mut M,
    [mut at, ch, n, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    // Windows 11 x86 truncates the search character to its byte/wide unit.
    let ch = ch & if WIDTH == 1 { 255 } else { 65535 };
    let mut left = if TERMINATED { u32::MAX } else { n };
    let mut found = 0;
    while left != 0 {
        let (pointer, count) = span(mem, at, WIDTH, left)?;
        for i in 0..count {
            // SAFETY: i indexes a checked unit.
            let x = unsafe { unit(pointer.add((i * WIDTH) as usize), WIDTH) };
            if x == ch {
                found = at.wrapping_add(i * WIDTH);
                if !LAST {
                    return Ok(found.into());
                }
            }
            if TERMINATED && x == 0 {
                return Ok(found.into());
            }
        }
        left -= count;
        at = at.wrapping_add(count * WIDTH);
    }
    Ok(found.into())
}

fn copy_string<M: Mem, const WIDTH: u32, const APPEND: bool>(
    mem: &mut M,
    [dst, src, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let offset = if APPEND {
        mem.terminated(dst, WIDTH, u32::MAX)? * WIDTH
    } else {
        0
    };
    let count = mem.terminated(src, WIDTH, u32::MAX)? + 1;
    let [from, to] = mem.spans([
        (src, count * WIDTH, false),
        (dst.wrapping_add(offset), count * WIDTH, true),
    ])?;
    // Windows 11 x86 strcpy/wcscpy also copy an overlapping source tail
    // into its preceding destination ("qAB" + 1 -> "AB").
    // SAFETY: both complete spans are checked; copies proceed in unit order.
    unsafe {
        for i in 0..count {
            let at = (i * WIDTH) as usize;
            if WIDTH == 1 {
                to.add(at).write(from.add(at).read());
            } else {
                to.add(at)
                    .cast::<u16>()
                    .write_unaligned(from.add(at).cast::<u16>().read_unaligned());
            }
        }
    }
    mem.commit()?;
    Ok(dst.into())
}

// Two-Way search over unaligned UTF-16 units. A byte search cannot skip
// unaligned matches: its non-overlapping iterator can skip a valid wide one.
pub(super) fn wide_substring(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let (h, n) = (haystack.len() / 2, needle.len() / 2);
    let get = |bytes: &[u8], i: usize| u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]);
    let suffix = |reverse: bool| {
        let (mut left, mut right, mut offset, mut period) = (-1isize, 0usize, 1usize, 1usize);
        while right + offset < n {
            let a = get(needle, (left + offset as isize) as usize);
            let b = get(needle, right + offset);
            if a == b {
                if offset == period {
                    right += period;
                    offset = 1;
                } else {
                    offset += 1;
                }
            } else if (a > b) != reverse {
                right += offset;
                offset = 1;
                period = (right as isize - left) as usize;
            } else {
                left = right as isize;
                right += 1;
                offset = 1;
                period = 1;
            }
        }
        (left, period)
    };
    let (a, ap) = suffix(false);
    let (b, bp) = suffix(true);
    let (critical, mut period) = if a > b {
        ((a + 1) as usize, ap)
    } else {
        ((b + 1) as usize, bp)
    };
    let remembered = if period + critical <= n
        && (0..critical).all(|i| get(needle, i) == get(needle, period + i))
    {
        n - period
    } else {
        period = critical.saturating_sub(1).max(n - critical) + 1;
        0
    };
    let (mut at, mut memory) = (0, 0);
    while at + n <= h {
        let mut i = critical.max(memory);
        while i < n && get(needle, i) == get(haystack, at + i) {
            i += 1;
        }
        if i < n {
            at += i + 1 - critical;
            memory = 0;
            continue;
        }
        i = critical;
        while i > memory && get(needle, i - 1) == get(haystack, at + i - 1) {
            i -= 1;
        }
        if i <= memory {
            return Some(at * 2);
        }
        at += period;
        memory = remembered;
    }
    None
}

fn substring<M: Mem, const WIDTH: u32>(
    mem: &mut M,
    [haystack, needle, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let n = mem.terminated(needle, WIDTH, u32::MAX)?;
    if n == 0 {
        return Ok(haystack.into());
    }
    let h = mem.terminated(haystack, WIDTH, u32::MAX)?;
    if n > h {
        return Ok(0u32.into());
    }
    let x = mem.input(haystack, h * WIDTH)?;
    let y = mem.input(needle, n * WIDTH)?;
    // SAFETY: the complete strings were checked. Both searches are linear
    // and allocation-free; wide matches start on a unit.
    let (x, y) = unsafe {
        (
            std::slice::from_raw_parts(x, (h * WIDTH) as usize),
            std::slice::from_raw_parts(y, (n * WIDTH) as usize),
        )
    };
    let found = if WIDTH == 1 {
        memchr::memmem::find(x, y)
    } else {
        wide_substring(x, y)
    };
    Ok(found.map_or(0, |i| haystack.wrapping_add(i as u32)).into())
}

macro_rules! string_body {
    ($name:ident, $function:path, $argc:literal) => {
        adv_cpu::body!($name = $function, inputs: match $argc { 1 => &[Input::Stack(0)], 2 => &[Input::Stack(0), Input::Stack(1)], _ => ARGS }, outputs: EAX, pop: 0);
    };
}
string_body!(MEMCHR, find::<_, 1, false, false>, 3);
string_body!(STRLEN, length::<_, 1, false>, 1);
string_body!(STRNLEN, length::<_, 1, true>, 2);
string_body!(WCSLEN, length::<_, 2, false>, 1);
string_body!(WCSNLEN, length::<_, 2, true>, 2);
string_body!(STRCMP, compare_strings::<_, 1, false, true, true>, 2);
string_body!(STRNCMP, compare_strings::<_, 1, true, false, true>, 3);
string_body!(STRNCMP_SIGN, compare_strings::<_, 1, true, true, true>, 3);
string_body!(WCSCMP, compare_strings::<_, 2, false, true, true>, 2);
string_body!(WCSNCMP, compare_strings::<_, 2, true, false, true>, 3);
string_body!(STRCHR, find::<_, 1, true, false>, 2);
string_body!(STRRCHR, find::<_, 1, true, true>, 2);
string_body!(WCSCHR, find::<_, 2, true, false>, 2);
string_body!(WCSRCHR, find::<_, 2, true, true>, 2);
string_body!(STRCPY, copy_string::<_, 1, false>, 2);
string_body!(WCSCPY, copy_string::<_, 2, false>, 2);
string_body!(STRCAT, copy_string::<_, 1, true>, 2);
string_body!(WCSCAT, copy_string::<_, 2, true>, 2);
string_body!(STRSTR, substring::<_, 1>, 2);
string_body!(WCSSTR, substring::<_, 2>, 2);

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
    // Windows 11 x86: strcmp/wcscmp/memcmp return a sign in all three
    // system CRTs. strncmp alone differs: ntdll/msvcrt subtract, UCRT signs.
    // strstr/wcsstr return the haystack for an empty needle, even if empty.
    let table: &[(&str, u8, adv_cpu::body::Body, &[&str])] = &[
        ("memset", 3, MEMSET, ALL),
        ("memmove", 3, MEMMOVE, CRT),
        ("memcmp", 3, MEMCMP, ALL),
        ("memchr", 3, MEMCHR, ALL),
        ("strlen", 1, STRLEN, ALL),
        ("strnlen", 2, STRNLEN, WITH_STRNLEN),
        ("wcslen", 1, WCSLEN, ALL),
        ("wcsnlen", 2, WCSNLEN, WITH_STRNLEN),
        ("strcmp", 2, STRCMP, ALL),
        ("strncmp", 3, STRNCMP, DIFF_COMPARISONS),
        ("strncmp", 3, STRNCMP_SIGN, SIGN_COMPARISONS),
        ("wcscmp", 2, WCSCMP, ALL),
        ("wcsncmp", 3, WCSNCMP, ALL),
        ("strchr", 2, STRCHR, ALL),
        ("strrchr", 2, STRRCHR, ALL),
        ("wcschr", 2, WCSCHR, ALL),
        ("wcsrchr", 2, WCSRCHR, ALL),
        ("strcpy", 2, STRCPY, ALL),
        ("wcscpy", 2, WCSCPY, ALL),
        ("strcat", 2, STRCAT, ALL),
        ("wcscat", 2, WCSCAT, ALL),
        ("strstr", 2, STRSTR, ALL),
        ("wcsstr", 2, WCSSTR, ALL),
    ];
    let mut bodies = Vec::new();
    for &(name, argc, body, modules) in table {
        for module in modules {
            bodies.push(Body {
                name: format!("{module}!{name}").into(),
                conv: Conv::Cdecl { argc },
                body: None,
                handler: resume_original,
                direct: Some(body),
            });
        }
    }
    bodies
}
