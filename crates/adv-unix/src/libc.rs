//! The parts of the GNU C library that Wine's Unix libraries use, for an
//! i386 guest.
//!
//! Functions whose behavior ntdll.dll's C runtime exports share, such as
//! `memcpy` or `cos`, bind to those exports ([`shared_with_ntdll`]). The
//! heap is the process heap. The rest are host functions with glibc's
//! semantics. FreeType is built into the host; other shared libraries load
//! from Wine's Unix directory. Files opened as streams are absent.

use adv_cpu::backend::{GuestExt, Reg};
use adv_machine::layout::teb;
use adv_machine::{Conv, Env, Outcome};
use adv_nt::memory::{self, Memory};
use adv_nt::process::Ntdll;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use crate::Function;
use crate::loader::{self, Libraries};

/// The longest format or string argument read.
const MAX_STRING: usize = 1 << 16;
const PEB_PROCESS_HEAP: u32 = 0x18;
const HEAP_ZERO_MEMORY: u32 = 8;
const RTLD_DEFAULT: u32 = 0;

/// The ntdll.dll export that implements C library function `name`.
pub fn shared_with_ntdll(name: &str) -> Option<&'static str> {
    const SHARED: &[&str] = &[
        "memchr", "memcmp", "memcpy", "memmove", "memset", "strcat", "strchr", "strcmp", "strcpy",
        "strlen", "strncmp", "strncpy", "strrchr", "strstr", "strtol", "qsort", "atan2", "cos",
        "sin", "floor", "pow", "log",
    ];
    match name {
        // C23 scanf differs from C89's only in binary integers.
        "__isoc23_sscanf" => Some("sscanf"),
        _ => SHARED.iter().find(|&&shared| shared == name).copied(),
    }
}

pub fn function(name: &str) -> Option<Function> {
    let cdecl = |argc| Conv::Cdecl { argc };
    let variadic = |fixed| Conv::CdeclVariadic { fixed };
    Some(match name {
        "malloc" => (cdecl(1), malloc),
        "calloc" => (cdecl(2), calloc),
        "realloc" => (cdecl(2), realloc),
        "free" => (cdecl(1), free),
        "strdup" => (cdecl(1), strdup),
        "snprintf" => (variadic(3), snprintf),
        "vsnprintf" => (cdecl(4), vsnprintf),
        "asprintf" => (variadic(2), asprintf),
        "__ctype_b_loc" => (cdecl(0), ctype_b_loc),
        "__ctype_toupper_loc" => (cdecl(0), ctype_toupper_loc),
        "_setjmp" => (cdecl(1), setjmp),
        "longjmp" => (cdecl(2), longjmp),
        "getenv" => (cdecl(1), getenv),
        "gethostname" => (cdecl(2), gethostname),
        "getaddrinfo" => (cdecl(4), getaddrinfo),
        "hypot" => (cdecl(4), hypot),
        "roundf" => (cdecl(1), roundf),
        "sincos" => (cdecl(4), sincos),
        "fopen64" => (cdecl(2), fopen),
        "dlopen" => (cdecl(2), dlopen),
        "dlsym" => (cdecl(2), dlsym),
        "dlclose" => (cdecl(1), dlclose),
        "__assert_fail" => (cdecl(4), assert_fail),
        _ => return None,
    })
}

fn string(env: &Env<'_>, address: u32, max: usize) -> Result<Vec<u8>, String> {
    env.cpu
        .read_cstr(address, max)
        .map_err(|error| format!("string at {address:#x}: {error}"))
}

// The heap.

/// Calls ntdll.dll's heap function `name` on the process heap with
/// `args` after the heap, and passes on its result.
pub(crate) fn heap_call(
    env: &mut Env<'_>,
    name: &str,
    args: &[u32],
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let teb = env.sys.contexts[env.context as usize].teb;
    let heap = env
        .cpu
        .read_u32(teb + teb::PEB)
        .and_then(|peb| env.cpu.read_u32(peb + PEB_PROCESS_HEAP));
    let function = env.ext.try_get::<Ntdll>().and_then(|ntdll| ntdll.get(name));
    let (Ok(heap), Some(function)) = (heap, function) else {
        return Outcome::Fail(format!("{name}: no process heap"));
    };
    let mut call = vec![heap];
    call.extend_from_slice(args);
    Outcome::call_aligned(env, function, call, move |env, returned| {
        then(env, returned.value())
    })
}

fn malloc(env: &mut Env<'_>) -> Outcome {
    let size = env.arg(0);
    heap_call(env, "RtlAllocateHeap", &[0, size], |_, block| {
        Outcome::Return(block)
    })
}

fn calloc(env: &mut Env<'_>) -> Outcome {
    let Some(size) = env.arg(0).checked_mul(env.arg(1)) else {
        return Outcome::Return(0);
    };
    heap_call(
        env,
        "RtlAllocateHeap",
        &[HEAP_ZERO_MEMORY, size],
        |_, block| Outcome::Return(block),
    )
}

fn realloc(env: &mut Env<'_>) -> Outcome {
    let (block, size) = (env.arg(0), env.arg(1));
    if block == 0 {
        return heap_call(env, "RtlAllocateHeap", &[0, size], |_, block| {
            Outcome::Return(block)
        });
    }
    if size == 0 {
        return heap_call(env, "RtlFreeHeap", &[0, block], |_, _| Outcome::Return(0));
    }
    heap_call(env, "RtlReAllocateHeap", &[0, block, size], |_, block| {
        Outcome::Return(block)
    })
}

fn free(env: &mut Env<'_>) -> Outcome {
    match env.arg(0) {
        0 => Outcome::Return(0),
        block => heap_call(env, "RtlFreeHeap", &[0, block], |_, _| Outcome::Return(0)),
    }
}

/// Copies `bytes` and a terminating zero into a new heap block.
pub(crate) fn allocate_string(
    env: &mut Env<'_>,
    mut bytes: Vec<u8>,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    bytes.push(0);
    heap_call(
        env,
        "RtlAllocateHeap",
        &[0, bytes.len() as u32],
        move |env, block| {
            if block != 0 && env.cpu.write(block, &bytes).is_err() {
                return Outcome::Fail(format!("heap block at {block:#x} is not writable"));
            }
            then(env, block)
        },
    )
}

fn strdup(env: &mut Env<'_>) -> Outcome {
    match string(env, env.arg(0), usize::MAX) {
        Ok(bytes) => allocate_string(env, bytes, |_, block| Outcome::Return(block)),
        Err(error) => Outcome::Fail(format!("strdup: {error}")),
    }
}

// Formatted output.

/// The arguments after `fixed` of a variadic call.
fn variadic_args(env: &Env<'_>, fixed: u32) -> u32 {
    env.cpu.reg(Reg::Esp) + 4 + fixed * 4
}

fn snprintf(env: &mut Env<'_>) -> Outcome {
    let args = variadic_args(env, 3);
    print_to(env, env.arg(0), env.arg(1), env.arg(2), args)
}

fn vsnprintf(env: &mut Env<'_>) -> Outcome {
    print_to(env, env.arg(0), env.arg(1), env.arg(2), env.arg(3))
}

/// Formats into `buffer` as much as fits in `size` with a terminating zero,
/// and returns the length of the whole output.
fn print_to(env: &mut Env<'_>, buffer: u32, size: u32, format: u32, args: u32) -> Outcome {
    let mut text = match print(env, format, args) {
        Ok(text) => text,
        Err(error) => return Outcome::Fail(format!("printf: {error}")),
    };
    let length = text.len() as u32;
    if size > 0 {
        text.truncate(size as usize - 1);
        text.push(0);
        if env.cpu.write(buffer, &text).is_err() {
            return Outcome::Fail(format!("printf: buffer at {buffer:#x} is not writable"));
        }
    }
    Outcome::Return(length)
}

fn asprintf(env: &mut Env<'_>) -> Outcome {
    let (result, args) = (env.arg(0), variadic_args(env, 2));
    let text = match print(env, env.arg(1), args) {
        Ok(text) => text,
        Err(error) => return Outcome::Fail(format!("asprintf: {error}")),
    };
    let length = text.len() as u32;
    allocate_string(env, text, move |env, block| {
        if block == 0 {
            return Outcome::Return(u32::MAX);
        }
        match env.cpu.write_u32(result, block) {
            Ok(()) => Outcome::Return(length),
            Err(_) => Outcome::Fail(format!("asprintf: result at {result:#x} is not writable")),
        }
    })
}

/// `format` with the arguments at `args`, as glibc's printf formats the
/// integer, fixed-point, character, string and pointer conversions.
fn print(env: &mut Env<'_>, format: u32, mut args: u32) -> Result<Vec<u8>, String> {
    let format = string(env, format, MAX_STRING)?;
    let mut next = |env: &Env<'_>| {
        let value = env
            .cpu
            .read_u32(args)
            .map_err(|error| format!("argument at {args:#x}: {error}"));
        args += 4;
        value
    };
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(&c) = format.get(at) {
        at += 1;
        if c != b'%' {
            out.push(c);
            continue;
        }
        let (mut left, mut plus, mut space, mut alternate, mut zero) =
            (false, false, false, false, false);
        while let Some(&flag) = format.get(at) {
            match flag {
                b'-' => left = true,
                b'+' => plus = true,
                b' ' => space = true,
                b'#' => alternate = true,
                b'0' => zero = true,
                _ => break,
            }
            at += 1;
        }
        let number = |at: &mut usize| {
            let start = *at;
            while format.get(*at).is_some_and(u8::is_ascii_digit) {
                *at += 1;
            }
            std::str::from_utf8(&format[start..*at])
                .unwrap()
                .parse()
                .unwrap_or(0usize)
        };
        let width = if format.get(at) == Some(&b'*') {
            at += 1;
            let width = next(env)? as i32;
            left |= width < 0;
            width.unsigned_abs() as usize
        } else {
            number(&mut at)
        };
        let precision = if format.get(at) == Some(&b'.') {
            at += 1;
            if format.get(at) == Some(&b'*') {
                at += 1;
                usize::try_from(next(env)? as i32).ok()
            } else {
                Some(number(&mut at))
            }
        } else {
            None
        };
        let start = at;
        while format.get(at).is_some_and(|c| b"hlLqjzt".contains(c)) {
            at += 1;
        }
        let length = &format[start..at];
        let wide = matches!(length, b"ll" | b"L" | b"q" | b"j");
        let Some(&conversion) = format.get(at) else {
            break;
        };
        at += 1;

        let pad = |out: &mut Vec<u8>, body: &[u8], prefix: &[u8], zeros: bool| {
            let fill = width.saturating_sub(prefix.len() + body.len());
            if left {
                out.extend_from_slice(prefix);
                out.extend_from_slice(body);
                out.resize(out.len() + fill, b' ');
            } else if zeros {
                out.extend_from_slice(prefix);
                out.resize(out.len() + fill, b'0');
                out.extend_from_slice(body);
            } else {
                out.resize(out.len() + fill, b' ');
                out.extend_from_slice(prefix);
                out.extend_from_slice(body);
            }
        };
        match conversion {
            b'%' => out.push(b'%'),
            b'f' | b'F' if length.is_empty() || length == b"l" => {
                let low = next(env)?;
                let value = f64::from_bits(u64::from(low) | u64::from(next(env)?) << 32);
                let format = format!(
                    "%{}{}{}{}{}*.*{}\0",
                    if left { "-" } else { "" },
                    if plus { "+" } else { "" },
                    if space { " " } else { "" },
                    if alternate { "#" } else { "" },
                    if zero { "0" } else { "" },
                    conversion as char,
                );
                let width = i32::try_from(width).map_err(|_| "floating width exceeds INT_MAX")?;
                let precision = i32::try_from(precision.unwrap_or(6))
                    .map_err(|_| "floating precision exceeds INT_MAX")?;
                // SAFETY: the generated C format takes two ints and one double.
                let size = unsafe {
                    ::libc::snprintf(
                        std::ptr::null_mut(),
                        0,
                        format.as_ptr().cast(),
                        width,
                        precision,
                        value,
                    )
                };
                let size = usize::try_from(size).map_err(|_| "floating conversion failed")?;
                let mut text = vec![0u8; size + 1];
                // SAFETY: text holds the measured output and its terminating NUL.
                unsafe {
                    ::libc::snprintf(
                        text.as_mut_ptr().cast(),
                        text.len(),
                        format.as_ptr().cast(),
                        width,
                        precision,
                        value,
                    );
                }
                out.extend_from_slice(&text[..size]);
            }
            b'd' | b'i' | b'u' | b'x' | b'X' | b'o' => {
                let raw = if wide {
                    let low = next(env)?;
                    u64::from(low) | u64::from(next(env)?) << 32
                } else {
                    u64::from(next(env)?)
                };
                let signed = matches!(conversion, b'd' | b'i');
                let value: i128 = match (length, signed) {
                    (b"hh", true) => (raw as i8).into(),
                    (b"hh", false) => (raw as u8).into(),
                    (b"h", true) => (raw as i16).into(),
                    (b"h", false) => (raw as u16).into(),
                    (_, true) if wide => (raw as i64).into(),
                    (_, true) => (raw as i32).into(),
                    (_, false) if wide => raw.into(),
                    (_, false) => (raw as u32).into(),
                };
                let magnitude = value.unsigned_abs();
                let mut digits = match conversion {
                    b'x' => format!("{magnitude:x}"),
                    b'X' => format!("{magnitude:X}"),
                    b'o' => format!("{magnitude:o}"),
                    _ => magnitude.to_string(),
                }
                .into_bytes();
                if let Some(precision) = precision {
                    if precision == 0 && magnitude == 0 {
                        digits.clear();
                    }
                    if digits.len() < precision {
                        digits.splice(0..0, std::iter::repeat_n(b'0', precision - digits.len()));
                    }
                }
                let prefix: &[u8] = match conversion {
                    _ if value < 0 => b"-",
                    b'd' | b'i' if plus => b"+",
                    b'd' | b'i' if space => b" ",
                    b'x' if alternate && magnitude != 0 => b"0x",
                    b'X' if alternate && magnitude != 0 => b"0X",
                    b'o' if alternate && digits.first() != Some(&b'0') => b"0",
                    _ => b"",
                };
                pad(&mut out, &digits, prefix, zero && precision.is_none());
            }
            b'c' => {
                let c = next(env)? as u8;
                pad(&mut out, &[c], b"", false);
            }
            b's' => {
                let address = next(env)?;
                let mut text = if address == 0 {
                    b"(null)".to_vec()
                } else {
                    string(env, address, precision.unwrap_or(MAX_STRING))?
                };
                if let Some(precision) = precision {
                    text.truncate(precision);
                }
                pad(&mut out, &text, b"", false);
            }
            b'p' => {
                let address = next(env)?;
                if address == 0 {
                    pad(&mut out, b"(nil)", b"", false);
                } else {
                    pad(&mut out, format!("{address:x}").as_bytes(), b"0x", false);
                }
            }
            b'n' => {
                let address = next(env)?;
                env.cpu
                    .write_u32(address, out.len() as u32)
                    .map_err(|error| format!("%n at {address:#x}: {error}"))?;
            }
            other => return Err(format!("unsupported conversion %{}", other as char)),
        }
    }
    Ok(out)
}

// Character classes of the C locale.

const ISUPPER: u16 = 0x100;
const ISLOWER: u16 = 0x200;
const ISALPHA: u16 = 0x400;
const ISDIGIT: u16 = 0x800;
const ISXDIGIT: u16 = 0x1000;
const ISSPACE: u16 = 0x2000;
const ISPRINT: u16 = 0x4000;
const ISGRAPH: u16 = 0x8000;
const ISBLANK: u16 = 0x1;
const ISCNTRL: u16 = 0x2;
const ISPUNCT: u16 = 0x4;
const ISALNUM: u16 = 0x8;

/// Where the pointers `__ctype_b_loc` and `__ctype_toupper_loc` return
/// are; each points at the entry for 0 of a table from -128 to 255.
#[derive(Clone, Copy)]
struct Ctype {
    classes: u32,
    upper: u32,
}

fn classes(c: u8) -> u16 {
    let mut classes = 0;
    for (test, class) in [
        (c.is_ascii_uppercase(), ISUPPER | ISALPHA | ISALNUM),
        (c.is_ascii_lowercase(), ISLOWER | ISALPHA | ISALNUM),
        (c.is_ascii_digit(), ISDIGIT | ISALNUM),
        (c.is_ascii_hexdigit(), ISXDIGIT),
        (b" \t\n\x0b\x0c\r".contains(&c), ISSPACE),
        (c == b' ' || c == b'\t', ISBLANK),
        (c.is_ascii_control(), ISCNTRL),
        (c.is_ascii_graphic(), ISGRAPH | ISPRINT),
        (c == b' ', ISPRINT),
        (c.is_ascii_punctuation(), ISPUNCT),
    ] {
        if test {
            classes |= class;
        }
    }
    classes
}

fn ctype(env: &mut Env<'_>) -> Result<Ctype, String> {
    if let Some(&ctype) = env.ext.try_get::<Ctype>() {
        return Ok(ctype);
    }
    // Both tables, then the two pointers.
    let (upper_at, pointers_at) = (384 * 2, 384 * 6);
    let mut bytes = vec![0; pointers_at + 8];
    for index in 0..384 {
        let c = (index as i32 - 128) as u8;
        bytes[index * 2..index * 2 + 2].copy_from_slice(&classes(c).to_le_bytes());
        let upper = if (index as i32 - 128) < 0 {
            index as i32 - 128
        } else {
            c.to_ascii_uppercase().into()
        };
        bytes[upper_at + index * 4..upper_at + index * 4 + 4].copy_from_slice(&upper.to_le_bytes());
    }
    let memory = env.ext.get_or_default::<Memory>();
    let base = memory::allocate_committed(env.cpu, &mut env.sys.vm, memory, bytes.len() as u32)
        .ok_or("no memory for the character tables")?;
    let classes = base + 128 * 2;
    let upper = base + upper_at as u32 + 128 * 4;
    bytes[pointers_at..pointers_at + 4].copy_from_slice(&classes.to_le_bytes());
    bytes[pointers_at + 4..].copy_from_slice(&upper.to_le_bytes());
    env.cpu
        .write(base, &bytes)
        .map_err(|error| error.to_string())?;
    let ctype = Ctype {
        classes: base + pointers_at as u32,
        upper: base + pointers_at as u32 + 4,
    };
    env.ext.insert(ctype);
    Ok(ctype)
}

fn ctype_b_loc(env: &mut Env<'_>) -> Outcome {
    match ctype(env) {
        Ok(ctype) => Outcome::Return(ctype.classes),
        Err(error) => Outcome::Fail(error),
    }
}

fn ctype_toupper_loc(env: &mut Env<'_>) -> Outcome {
    match ctype(env) {
        Ok(ctype) => Outcome::Return(ctype.upper),
        Err(error) => Outcome::Fail(error),
    }
}

/// _setjmp(env): glibc's i386 `jmp_buf` holds EBX, ESI, EDI, EBP, the
/// caller's ESP and the return address; the signal mask is not saved.
fn setjmp(env: &mut Env<'_>) -> Outcome {
    let (buffer, esp) = (env.arg(0), env.cpu.reg(Reg::Esp));
    let Ok(pc) = env.cpu.read_u32(esp) else {
        return Outcome::Fail("_setjmp: unreadable return address".into());
    };
    let words = [
        env.cpu.reg(Reg::Ebx),
        env.cpu.reg(Reg::Esi),
        env.cpu.reg(Reg::Edi),
        env.cpu.reg(Reg::Ebp),
        esp + 4,
        pc,
        0,
    ];
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    match env.cpu.write(buffer, &bytes) {
        Ok(()) => Outcome::Return(0),
        Err(_) => Outcome::Fail(format!("_setjmp: buffer at {buffer:#x} is not writable")),
    }
}

/// longjmp(env, value): back to the `_setjmp` that filled the buffer,
/// returning `value`, or 1 for 0.
fn longjmp(env: &mut Env<'_>) -> Outcome {
    let (buffer, value) = (env.arg(0), env.arg(1).max(1));
    let mut words = [0u32; 6];
    for (index, word) in words.iter_mut().enumerate() {
        match env.cpu.read_u32(buffer + 4 * index as u32) {
            Ok(value) => *word = value,
            Err(_) => return Outcome::Fail(format!("longjmp: unreadable buffer at {buffer:#x}")),
        }
    }
    let [ebx, esi, edi, ebp, esp, eip] = words;
    let mut checkpoint = match env.checkpoint_for_stack_unwind(esp) {
        Ok(checkpoint) => checkpoint,
        Err(reason) => return Outcome::Fail(format!("longjmp: {reason}")),
    };
    let regs = &mut checkpoint.state.regs;
    (regs[0], regs[3], regs[4], regs[5], regs[6], regs[7]) = (value, ebx, esp, ebp, esi, edi);
    checkpoint.state.eip = eip;
    Outcome::ResumeExecution(checkpoint.unwind_callbacks())
}

/// The process has no Unix environment.
fn getenv(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(0)
}

/// The machine is always the prefix's computer, never the host.
fn gethostname(env: &mut Env<'_>) -> Outcome {
    let (buffer, size) = (env.arg(0), env.arg(1) as usize);
    let mut name = adv_nt::environment::COMPUTER_NAME.as_bytes().to_vec();
    name.push(0);
    if name.len() > size || env.cpu.write(buffer, &name).is_err() {
        return Outcome::Return(u32::MAX);
    }
    Outcome::Return(0)
}

/// glibc's `EAI_NONAME`.
const EAI_NONAME: i32 = -2;

/// A session resolves no names: the guest sees a machine without a network.
fn getaddrinfo(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(EAI_NONAME as u32)
}

// Mathematics.

fn f64_arg(env: &Env<'_>, index: u32) -> f64 {
    f64::from_bits(u64::from(env.arg(index)) | u64::from(env.arg(index + 1)) << 32)
}

fn hypot(env: &mut Env<'_>) -> Outcome {
    Outcome::ReturnF64(f64_arg(env, 0).hypot(f64_arg(env, 2)))
}

fn roundf(env: &mut Env<'_>) -> Outcome {
    Outcome::ReturnF32(f32::from_bits(env.arg(0)).round())
}

fn sincos(env: &mut Env<'_>) -> Outcome {
    let (sin, cos) = f64_arg(env, 0).sin_cos();
    let (sin_at, cos_at) = (env.arg(2), env.arg(3));
    if env.cpu.write(sin_at, &sin.to_le_bytes()).is_err()
        || env.cpu.write(cos_at, &cos.to_le_bytes()).is_err()
    {
        return Outcome::Fail("sincos: results are not writable".into());
    }
    Outcome::Return(0)
}

// What the host lacks.

/// The process opens no Unix streams.
fn fopen(env: &mut Env<'_>) -> Outcome {
    if let Ok(path) = string(env, env.arg(0), MAX_STRING) {
        log::debug!("fopen {}: absent", String::from_utf8_lossy(&path));
    }
    Outcome::Return(0)
}

/// dlopen(name, flags): the shared libraries present are those next to
/// the Unix libraries of Wine's modules, or the built-in host FreeType.
fn dlopen(env: &mut Env<'_>) -> Outcome {
    let name = match string(env, env.arg(0), MAX_STRING) {
        Ok(name) => PathBuf::from(OsString::from_vec(name)),
        Err(error) => return Outcome::Fail(format!("dlopen: {error}")),
    };
    if name == Path::new(crate::freetype::SONAME) {
        return Outcome::Return(crate::freetype::HANDLE);
    }
    let libraries = env.ext.get_or_default::<Libraries>();
    let directory = libraries.0.first().and_then(|first| first.path.parent());
    let path = match directory {
        Some(directory) if name.parent() == Some(Path::new("")) => directory.join(&name),
        _ => name,
    };
    let shown = path.clone();
    loader::open(
        env,
        &shown,
        Box::new(move |_, base| {
            if base.is_err() {
                log::debug!("dlopen {}: absent", path.display());
            }
            Outcome::Return(base.unwrap_or(0))
        }),
    )
}

/// dlsym(handle, name): a symbol of the library `handle` names, or of any
/// for RTLD_DEFAULT.
fn dlsym(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    let name = match string(env, env.arg(1), MAX_STRING) {
        Ok(name) => String::from_utf8_lossy(&name).into_owned(),
        Err(error) => return Outcome::Fail(format!("dlsym: {error}")),
    };
    if handle == crate::freetype::HANDLE {
        if crate::freetype::function(&name).is_none() {
            return Outcome::Return(0);
        }
        return match env
            .sys
            .thunks
            .resolve(env.cpu, crate::freetype::SONAME, &name)
        {
            Ok(address) => Outcome::Return(address),
            _ => Outcome::Return(0),
        };
    }
    let libraries = &env.ext.get_or_default::<Libraries>().0;
    let address = libraries
        .iter()
        .filter(|library| handle == RTLD_DEFAULT || library.base == handle)
        .find_map(|library| library.symbol(&name));
    Outcome::Return(address.unwrap_or(0))
}

fn dlclose(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(0)
}

/// __assert_fail(assertion, file, line, function)
fn assert_fail(env: &mut Env<'_>) -> Outcome {
    let text = |env: &Env<'_>, address| {
        string(env, address, MAX_STRING)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    };
    Outcome::Fail(format!(
        "{}:{}: {}: assertion failed: {}",
        text(env, env.arg(1)),
        env.arg(2),
        text(env, env.arg(3)),
        text(env, env.arg(0)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{
        Cpu,
        backend::{Guest, Prot},
    };
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};
    #[cfg(any(target_vendor = "apple", target_os = "android"))]
    use adv_wineserver as _;

    // spec: WINE-UNIX-5
    #[test]
    fn fixed_point_arguments_and_bounded_output() {
        let mut machine = Machine::new(
            Cpu::new(PARK_ADDR, false).unwrap(),
            crate::Platform,
            Clock::virtual_time(),
        )
        .unwrap();
        let (format, args, buffer) = (0x400000, 0x400100, 0x400200);
        machine.cpu.map(format, 0x1000, Prot::RW).unwrap();
        for (spec, value, expected) in [
            ("%f", 1.25, "1.250000"),
            ("%+.2f", 1.25, "+1.25"),
            ("%08.2f", -1.25, "-0001.25"),
            ("%-8.2lf", 1.25, "1.25    "),
            ("%#.0f", 2.0, "2."),
            ("% f", -0.0, "-0.000000"),
            ("%08f", f64::INFINITY, "     inf"),
            ("%F", f64::NAN, "NAN"),
        ] {
            machine
                .cpu
                .write(format, format!("{spec}:%u\0").as_bytes())
                .unwrap();
            machine.cpu.write(args, &value.to_le_bytes()).unwrap();
            machine.cpu.write_u32(args + 8, 37).unwrap();
            machine.visit_context(None, |env| {
                let expected = format!("{expected}:37");
                assert_eq!(print(env, format, args).unwrap(), expected.as_bytes());
                assert!(matches!(print_to(env, buffer, 4, format, args), Outcome::Return(n) if n == expected.len() as u32));
                let mut bytes = [0; 4];
                env.cpu.read(buffer, &mut bytes).unwrap();
                assert_eq!(&bytes[..3], &expected.as_bytes()[..3]);
                assert_eq!(bytes[3], 0);
            }).unwrap();
        }
        machine.cpu.write(format, b"%*.*f:%u\0").unwrap();
        machine.cpu.write_u32(args, (-8i32) as u32).unwrap();
        machine.cpu.write_u32(args + 4, 2).unwrap();
        machine.cpu.write(args + 8, &1.25f64.to_le_bytes()).unwrap();
        machine.cpu.write_u32(args + 16, 37).unwrap();
        assert_eq!(
            machine
                .visit_context(None, |env| print(env, format, args))
                .unwrap()
                .unwrap(),
            b"1.25    :37"
        );
    }
}
