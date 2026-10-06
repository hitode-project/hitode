//! Small C-runtime services used by guest library DLLs. Library algorithms remain guest code.
use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Reg};
use adv_machine::{Conv, Env, Outcome};
use adv_os::Heaps;
use std::collections::HashMap;
mod convert;
mod cpp_alloc;
mod data;
mod exceptions;
pub(crate) use data::initialize as initialize_data;
mod floating;
mod heap;
mod locale;
mod onexit;
mod secure_memory;
mod startup;
mod wide_string;

#[derive(Default)]
struct CrtState {
    errno: HashMap<(String, u32), u32>,
    locks: HashMap<(String, u32), u32>,
    globals: HashMap<(String, String), u32>,
    invalid_handlers: HashMap<String, u32>,
    files: HashMap<u32, std::fs::File>,
    locales: HashMap<String, locale::LocaleState>,
}

// API-set imports and their UCRT exports share the same CRT state owner.
fn crt_module(module: &str) -> &str {
    if module.starts_with("api-ms-win-crt-") {
        "ucrtbase.dll"
    } else {
        module
    }
}

fn invalid_parameter(env: &mut Env<'_>, function: &str, error: i32) -> Outcome {
    invalid_parameter_result(env, function, error, error as u32)
}

fn invalid_parameter_result(
    env: &mut Env<'_>,
    function: &str,
    error: i32,
    result: u32,
) -> Outcome {
    let errno = floating::errno_address(env);
    if errno.is_null() || errno.write(env.cpu, error).is_err() {
        return Outcome::Fail("CRT errno allocation/write failed".into());
    }
    let handler = env
        .ext
        .get::<CrtState>()
        .invalid_handlers
        .get(crt_module(&env.sys.thunks.get(env.thunk).module))
        .copied()
        .unwrap_or(0);
    if handler == 0 {
        return Outcome::Fail(format!(
            "{function}: invalid parameter without a returning guest handler"
        ));
    }
    // Release UCRT passes no diagnostic strings. errno is assigned before the
    // callback, and a returning callback may change it or wait via the Machine.
    Outcome::call(handler, [0, 0, 0, 0, 0], move |_, _| {
        Outcome::Return(result)
    })
}

pub fn install<B: adv_cpu::backend::Backend>(
    machine: &mut adv_machine::Machine<B>,
) -> Result<(), String> {
    machine.ext.insert(CrtState::default());
    Ok(())
}

pub fn register(r: &mut Registry) {
    data::register(r);
    exceptions::register(r);
    floating::register(r);
    onexit::register(r);
    startup::register(r);
    convert::register(r);
    cpp_alloc::register(r);
    secure_memory::register(r);
    heap::register(r);
    locale::register(r);
    wide_string::register(r);
    for (name, argc) in [
        ("malloc", 1),
        ("calloc", 2),
        ("realloc", 2),
        ("free", 1),
        ("memcpy", 3),
        ("memmove", 3),
        ("memset", 3),
        ("memcmp", 3),
        ("strlen", 1),
        ("strcmp", 2),
        ("_initterm", 2),
        ("_initterm_e", 2),
        ("_lock", 1),
        ("_unlock", 1),
        ("__dllonexit", 3),
        ("__set_app_type", 1),
        ("__p__fmode", 0),
        ("__p__commode", 0),
        ("toupper", 1),
        ("qsort", 4),
        ("floor", 2),
        ("ceil", 2),
        ("ldexp", 3),
        ("frexp", 3),
        ("_CIpow", 0),
        ("_ftol", 0),
        ("time", 1),
        ("_time32", 1),
        ("strncmp", 3),
        ("strchr", 2),
        ("strstr", 2),
        ("_strdup", 1),
        ("strtoul", 3),
        ("_mbsinc", 1),
        ("_mbsnbcpy", 3),
        ("_ismbblead", 1),
        ("_ismbbtrail", 1),
        ("_splitpath", 5),
        ("_makepath", 5),
        ("_beginthread", 3),
        ("_XcptFilter", 2),
        ("__setusermatherr", 1),
        ("getenv", 1),
        ("strrchr", 2),
        ("strcpy", 2),
        ("strncpy", 3),
        ("strcat", 2),
        ("strncat", 3),
        ("??_U@YAPAXI@Z", 1),
        ("??_V@YAXPAX@Z", 1),
        ("wcslen", 1),
        ("wcscmp", 2),
        ("wcsncmp", 3),
        ("wcschr", 2),
        ("wcsrchr", 2),
        ("wcscpy", 2),
        ("wcsncpy", 3),
        ("wcscat", 2),
        ("wcsstr", 2),
        ("_wcsicmp", 2),
        ("_wcsnicmp", 3),
        ("_wfopen_s", 3),
        ("_wfopen", 2),
        ("fopen_s", 3),
        ("fopen", 2),
        ("fread", 4),
        ("fwrite", 4),
        ("fseek", 3),
        ("ftell", 1),
        ("fclose", 1),
        ("fflush", 1),
        ("feof", 1),
        ("strncpy_s", 4),
        ("strcpy_s", 3),
        ("strcat_s", 3),
        ("strncat_s", 4),
        ("wcsncpy_s", 4),
        ("wcscpy_s", 3),
        ("wcscat_s", 3),
    ] {
        r.add("msvcrt.dll", name, Conv::Cdecl { argc }, call);
    }
    // Whitelist implemented x86 cdecl constructor and heap helpers. Unknown
    // functions keep the loader's explicit unsupported handler.
    for module in ["msvcr80.dll", "msvcr90.dll"] {
        for name in ["_initterm", "_initterm_e"] {
            r.add(module, name, Conv::Cdecl { argc: 2 }, call);
        }
        for name in ["_lock", "_unlock"] {
            r.add(module, name, Conv::Cdecl { argc: 1 }, call);
        }
        r.add(module, "__dllonexit", Conv::Cdecl { argc: 3 }, call);
        for (name, argc) in [
            ("__set_app_type", 1),
            ("__p__fmode", 0),
            ("__p__commode", 0),
            ("__iob_func", 0),
        ] {
            r.add(module, name, Conv::Cdecl { argc }, call);
        }
        for (name, argc) in [
            ("_malloc_crt", 1),
            ("malloc", 1),
            ("calloc", 2),
            ("realloc", 2),
            ("free", 1),
        ] {
            r.add(module, name, Conv::Cdecl { argc }, call);
        }
        for (name, argc) in [
            ("_encode_pointer", 1),
            ("_decode_pointer", 1),
            ("_encoded_null", 0),
            ("time", 1),
            ("_time32", 1),
            ("_ismbblead", 1),
            ("_ismbbtrail", 1),
            ("memcpy", 3),
            ("memmove", 3),
            ("memset", 3),
            ("memcmp", 3),
            ("strlen", 1),
            ("strcmp", 2),
            ("strncmp", 3),
            ("strchr", 2),
            ("strrchr", 2),
            ("strstr", 2),
            ("_strdup", 1),
            ("strtoul", 3),
            ("_mbsinc", 1),
            ("_mbsnbcpy", 3),
            ("_splitpath", 5),
            ("_makepath", 5),
            ("getenv", 1),
            ("strcpy", 2),
            ("strncpy", 3),
            ("strcat", 2),
            ("strncat", 3),
            ("??_U@YAPAXI@Z", 1),
            ("??_V@YAXPAX@Z", 1),
            ("wcslen", 1),
            ("wcscmp", 2),
            ("wcsncmp", 3),
            ("wcschr", 2),
            ("wcsrchr", 2),
            ("wcscpy", 2),
            ("wcsncpy", 3),
            ("wcscat", 2),
            ("wcsstr", 2),
            ("_wcsicmp", 2),
            ("_wcsnicmp", 3),
            ("_wfopen_s", 3),
            ("_wfopen", 2),
            ("fopen_s", 3),
            ("fopen", 2),
            ("fread", 4),
            ("fwrite", 4),
            ("fseek", 3),
            ("ftell", 1),
            ("fclose", 1),
            ("fflush", 1),
            ("feof", 1),
            ("strncpy_s", 4),
            ("strcpy_s", 3),
            ("strcat_s", 3),
            ("strncat_s", 4),
            ("wcsncpy_s", 4),
            ("wcscpy_s", 3),
            ("wcscat_s", 3),
        ] {
            r.add(module, name, Conv::Cdecl { argc }, call);
        }
        r.add(module, "_setjmp3", adv_machine::Conv::CdeclVariadic { fixed: 2 }, call);
    }
    r.add("msvcrt.dll", "_setjmp3", adv_machine::Conv::CdeclVariadic { fixed: 2 }, call);
}
/// Release thread-owned cells only after the existing DLL/TLS detach callbacks.
pub(crate) fn detach_thread(env: &mut Env<'_>) -> Result<(), String> {
    release_errno(env, Some(env.context))
}

/// Host CRT modules are pinned. Process detach is their final errno boundary.
pub(crate) fn detach_process(env: &mut Env<'_>) -> Result<(), String> {
    onexit::release_process_tables(env)?;
    startup::release_process_tables(env)?;
    locale::release_process_tables(env)?;
    release_errno(env, None)
}

fn release_errno(env: &mut Env<'_>, context: Option<adv_machine::CtxId>) -> Result<(), String> {
    let Some(state) = env.ext.try_get::<CrtState>() else {
        return Ok(());
    };
    let cells: Vec<_> = state
        .errno
        .iter()
        .filter(|((_, id), _)| context.is_none_or(|ctx| *id == ctx))
        .map(|(key, &address)| (key.clone(), address))
        .collect();
    for (key, address) in cells {
        let heaps = env.ext.get_mut::<Heaps>();
        if address != 0 && !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, address) {
            return Err(format!("CRT errno detach: invalid heap cell {address:#x}"));
        }
        env.ext.get_mut::<CrtState>().errno.remove(&key);
    }
    Ok(())
}

fn double(e: &Env<'_>) -> Result<f64, String> {
    let mut b = [0; 8];
    e.cpu
        .read(e.cpu.reg(Reg::Esp).wrapping_add(4), &mut b)
        .map_err(|e| e.to_string())?;
    Ok(f64::from_le_bytes(b))
}
fn split_fraction(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        let (v, e) = split_fraction(x * 18014398509481984.0);
        return (v, e - 54);
    }
    (
        f64::from_bits((bits & 0x800fffffffffffff) | (1022u64 << 52)),
        exp - 1022,
    )
}
fn scale(mut x: f64, mut exp: i32) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    exp = exp.clamp(-4096, 4096);
    while exp > 512 {
        x *= 2f64.powi(512);
        exp -= 512;
    }
    while exp < -512 {
        x *= 2f64.powi(-512);
        exp += 512;
    }
    x * 2f64.powi(exp)
}
fn sort_step(
    _e: &mut Env<'_>,
    base: u32,
    count: u32,
    size: u32,
    cmp: u32,
    i: u32,
    j: u32,
) -> Outcome {
    if i >= count {
        return Outcome::Return(0);
    }
    let (i, j) = if j == 0 { (i + 1, i + 1) } else { (i, j) };
    if i >= count {
        return Outcome::Return(0);
    }
    let left = base + (j - 1) * size;
    let right = base + j * size;
    Outcome::call(cmp, [left, right], move |e, result| {
        if result.value() as i32 > 0 {
            let mut temporary = vec![0; size as usize];
            if let Err(err) = e
                .cpu
                .read(left, &mut temporary)
                .and_then(|_| e.cpu.copy_memory(right, left, size as usize))
                .and_then(|_| e.cpu.write(right, &temporary))
            {
                return Outcome::Fail(err.to_string());
            }
            sort_step(e, base, count, size, cmp, i, j - 1)
        } else {
            sort_step(e, base, count, size, cmp, i + 1, i + 1)
        }
    })
}
const CRT_ALLOCATION_LIMIT: u32 = 0x40000000;
fn allocate(e: &mut Env<'_>, size: u32, zero: bool) -> u32 {
    if size > CRT_ALLOCATION_LIMIT {
        return 0;
    }
    let heaps = e.ext.get_mut::<Heaps>();
    let h = heaps.process_heap;
    heaps
        .alloc(e.cpu, &mut e.sys.vm, h, size, zero)
        .unwrap_or(0)
}

fn file_open_options(mode: &str) -> Option<std::fs::OpenOptions> {
    let mut opts = std::fs::OpenOptions::new();
    let m = mode.trim_matches(['b', 't']);
    if m.starts_with("r+") || mode.contains('+') && mode.starts_with('r') {
        opts.read(true).write(true);
    } else if m.starts_with("w+") || mode.contains('+') && mode.starts_with('w') {
        opts.read(true).write(true).create(true).truncate(true);
    } else if m.starts_with("a+") || mode.contains('+') && mode.starts_with('a') {
        opts.read(true).append(true).create(true);
    } else if m.starts_with('r') {
        opts.read(true);
    } else if m.starts_with('w') {
        opts.write(true).create(true).truncate(true);
    } else if m.starts_with('a') {
        opts.append(true).create(true);
    } else {
        return None;
    }
    Some(opts)
}

fn crt_open_file(e: &mut Env<'_>, name: &str, mode: &str) -> Option<u32> {
    let opts = file_open_options(mode)?;
    let host_path = if mode.starts_with('w') || mode.starts_with('a') {
        e.ext.try_get::<crate::Win32>()?.vfs.write_path(name)?
    } else {
        e.ext.try_get::<crate::Win32>()?.vfs.lookup(name)?
    };
    let file = opts.open(&host_path).ok()?;
    let ptr = allocate(e, 32, true);
    if ptr == 0 {
        return None;
    }
    if e.ext.try_get::<CrtState>().is_none() {
        e.ext.insert(CrtState::default());
    }
    e.ext.get_mut::<CrtState>().files.insert(ptr, file);
    Some(ptr)
}

fn initializers(e: &mut Env<'_>, mut p: u32, end: u32, stop_on_error: bool) -> Outcome {
    while p < end {
        let Ok(target) = e.cpu.read_u32(p) else {
            return Outcome::Fail("CRT initializer table unmapped".into());
        };
        p += 4;
        if target != 0 {
            return Outcome::call(target, vec![], move |e, result| {
                if stop_on_error && result.value() != 0 {
                    Outcome::Return(result.value())
                } else {
                    initializers(e, p, end, stop_on_error)
                }
            });
        }
    }
    Outcome::Return(0)
}
fn split_path_a(path: &[u8]) -> (&[u8], &[u8], &[u8], &[u8]) {
    let (drive, rest) = if path.len() >= 2 && path[1] == b':' {
        (&path[..2], &path[2..])
    } else {
        (&[][..], path)
    };
    let last_slash = rest.iter().rposition(|&c| c == b'/' || c == b'\\');
    let (dir, file_part) = match last_slash {
        Some(idx) => (&rest[..=idx], &rest[idx + 1..]),
        None => (&[][..], rest),
    };
    let last_dot = file_part.iter().rposition(|&c| c == b'.');
    let (fname, ext) = match last_dot {
        Some(idx) => (&file_part[..idx], &file_part[idx..]),
        None => (file_part, &[][..]),
    };
    (drive, dir, fname, ext)
}
fn call(e: &mut Env<'_>) -> Outcome {
    let name = e.sys.thunks.get(e.thunk).name.clone();
    let arg = |i| e.arg(i);
    let (a, b, c) = (arg(0), arg(1), arg(2));
    let value = match name.as_str() {
        "__set_app_type" | "__p__fmode" | "__p__commode" | "__iob_func" => {
            if e.ext.try_get::<CrtState>().is_none() {
                e.ext.insert(CrtState::default());
            }
            let key = (
                e.sys.thunks.get(e.thunk).module.clone(),
                if name == "__set_app_type" {
                    "app_type"
                } else if name == "__iob_func" {
                    "iob"
                } else {
                    name.trim_start_matches("__p__")
                }
                .to_owned(),
            );
            let existing = e.ext.get::<CrtState>().globals.get(&key).copied();
            let pointer = if let Some(pointer) = existing {
                pointer
            } else {
                let size = if name == "__iob_func" { 128 } else { 4 };
                let pointer = allocate(e, size, true);
                if pointer == 0 {
                    return Outcome::Fail("CRT global allocation failed".into());
                }
                if name == "__p__fmode"
                    && let Err(error) = e.cpu.write_u32(pointer, 0x4000)
                {
                    return Outcome::Fail(error.to_string());
                }
                e.ext.get_mut::<CrtState>().globals.insert(key, pointer);
                pointer
            };
            if name == "__set_app_type" {
                if let Err(error) = e.cpu.write_u32(pointer, a) {
                    return Outcome::Fail(error.to_string());
                }
                0
            } else {
                pointer
            }
        }
        "__dllonexit" => {
            if !e.cpu.accessible(b, 4, adv_cpu::backend::Prot::RW)
                || !e.cpu.accessible(c, 4, adv_cpu::backend::Prot::RW)
            {
                return Outcome::Fail("CRT exit table pointer is inaccessible".into());
            }
            let (Ok(begin), Ok(end)) = (e.cpu.read_u32(b), e.cpu.read_u32(c)) else {
                return Outcome::Fail("CRT exit table pointer is unmapped".into());
            };
            let Some(used) = end
                .checked_sub(begin)
                .filter(|used| used.is_multiple_of(4) && *used < 1024 * 1024)
            else {
                return Outcome::Fail("CRT exit table range is invalid".into());
            };
            let needed = used + 4;
            let pointer = if begin == 0 {
                allocate(e, needed, false)
            } else {
                let heaps = e.ext.get_mut::<Heaps>();
                heaps
                    .realloc(
                        e.cpu,
                        &mut e.sys.vm,
                        heaps.process_heap,
                        begin,
                        needed,
                        false,
                        false,
                    )
                    .unwrap_or(0)
            };
            if pointer == 0 {
                return Outcome::Return(0);
            }
            if let Err(error) = e
                .cpu
                .write_u32(pointer + used, a)
                .and_then(|_| e.cpu.write_u32(b, pointer))
                .and_then(|_| e.cpu.write_u32(c, pointer + needed))
            {
                return Outcome::Fail(error.to_string());
            }
            a
        }
        "_lock" | "_unlock" => {
            if a >= 64 {
                return Outcome::Fail("CRT lock index outside the supported table".into());
            }
            if e.ext.try_get::<CrtState>().is_none() {
                e.ext.insert(CrtState::default());
            }
            let module = e.sys.thunks.get(e.thunk).module.clone();
            let state = e.ext.get_mut::<CrtState>();
            let handle = *state.locks.entry((module, a)).or_insert_with(|| {
                e.sys.objects.insert(adv_machine::Object::Mutex {
                    owner: None,
                    recursion: 0,
                })
            });
            if handle == 0 {
                return Outcome::Fail("CRT lock allocation exhausted the object table".into());
            }
            if name == "_lock" {
                if e.sys.try_acquire(handle) {
                    return Outcome::Return(0);
                }
                return Outcome::block(
                    adv_machine::Wait::Object {
                        handle,
                        deadline: None,
                    },
                    |_, _| Outcome::Return(0),
                );
            }
            match e.sys.objects.get_mut(handle) {
                Some(adv_machine::Object::Mutex { owner, recursion })
                    if *owner == Some(e.context) =>
                {
                    *recursion -= 1;
                    if *recursion == 0 {
                        *owner = None;
                    }
                }
                _ => return Outcome::Fail("CRT unlock by a non-owner".into()),
            }
            e.sys.signal_waiters(handle);
            0
        }
        "malloc" | "_malloc_crt" | "??_U@YAPAXI@Z" => allocate(e, a, false),
        "_encode_pointer" | "_decode_pointer" | "_encoded_null" => {
            let kernel = if name == "_decode_pointer" {
                "DecodePointer"
            } else {
                "EncodePointer"
            };
            let target = match e.sys.thunks.resolve(e.cpu, "kernel32.dll", kernel) {
                Ok(target) => target,
                Err(error) => return Outcome::Fail(error.to_string()),
            };
            return Outcome::call(
                target,
                [if name == "_encoded_null" { 0 } else { a }],
                |_, value| Outcome::Return(value.value()),
            );
        }
        "calloc" => a.checked_mul(b).map_or(0, |n| allocate(e, n, true)),
        "free" | "??_V@YAXPAX@Z" => {
            if a != 0 {
                let heaps = e.ext.get_mut::<Heaps>();
                heaps.free(e.cpu, &mut e.sys.vm, heaps.process_heap, a);
            }
            0
        }
        "realloc" => {
            if a == 0 {
                allocate(e, b, false)
            } else if b == 0 {
                let heaps = e.ext.get_mut::<Heaps>();
                heaps.free(e.cpu, &mut e.sys.vm, heaps.process_heap, a);
                0
            } else {
                let heaps = e.ext.get_mut::<Heaps>();
                heaps
                    .realloc(e.cpu, &mut e.sys.vm, heaps.process_heap, a, b, false, false)
                    .unwrap_or(0)
            }
        }
        "memcpy" | "memmove" => {
            if let Err(err) = e.cpu.copy_memory(b, a, c as usize) {
                return Outcome::Fail(err.to_string());
            }
            a
        }
        "memset" => {
            let chunk = [b as u8; 4096];
            let mut p = 0;
            while p < c {
                let n = (c - p).min(4096);
                if let Err(err) = e.cpu.write(a.wrapping_add(p), &chunk[..n as usize]) {
                    return Outcome::Fail(err.to_string());
                }
                p += n;
            }
            a
        }
        "memcmp" => {
            let mut left = [0; 4096];
            let mut right = [0; 4096];
            let mut offset = 0;
            let mut diff = 0;
            while offset < c {
                let n = (c - offset).min(4096) as usize;
                if let Err(err) = e
                    .cpu
                    .read(a.wrapping_add(offset), &mut left[..n])
                    .and_then(|_| e.cpu.read(b.wrapping_add(offset), &mut right[..n]))
                {
                    return Outcome::Fail(err.to_string());
                }
                if let Some((l, r)) = left[..n].iter().zip(&right[..n]).find(|(l, r)| l != r) {
                    diff = i32::from(*l) - i32::from(*r);
                    break;
                }
                offset += n as u32;
            }
            diff as u32
        }
        "strlen" => match e.cpu.read_cstr(a, 16 * 1024 * 1024) {
            Ok(s) => s.len() as u32,
            Err(err) => return Outcome::Fail(err.to_string()),
        },
        "strcmp" => {
            let (Ok(l), Ok(r)) = (
                e.cpu.read_cstr(a, 16 * 1024 * 1024),
                e.cpu.read_cstr(b, 16 * 1024 * 1024),
            ) else {
                return Outcome::Fail("CRT string unmapped".into());
            };
            match l.cmp(&r) {
                std::cmp::Ordering::Less => u32::MAX,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }
        }
        "_initterm" | "_initterm_e" => {
            if b < a || b - a > 1024 * 1024 || !(b - a).is_multiple_of(4) {
                return Outcome::Fail("invalid initializer range".into());
            }
            return initializers(e, a, b, name == "_initterm_e");
        }
        "toupper" => {
            if a <= 255 {
                u32::from((a as u8).to_ascii_uppercase())
            } else {
                a
            }
        }
        "qsort" => {
            if b < 2 || c == 0 {
                return Outcome::Return(0);
            }
            if c > 1024 * 1024 || b.checked_mul(c).and_then(|n| a.checked_add(n)).is_none() {
                return Outcome::Fail("invalid qsort range".into());
            }
            let cmp = e.arg(3);
            return sort_step(e, a, b, c, cmp, 1, 1);
        }
        "time" | "_time32" => {
            let ft = crate::kernel32::filetime_now(e);
            let unix_sec = (ft.saturating_sub(116_444_736_000_000_000) / 10_000_000) as u32;
            if a != 0 {
                if let Err(error) = e.cpu.write_u32(a, unix_sec) {
                    return Outcome::Fail(error.to_string());
                }
            }
            unix_sec
        }
        "strncmp" => {
            let n = c as usize;
            if n == 0 {
                return Outcome::Return(0);
            }
            let mut diff = 0i32;
            for i in 0..n {
                let Ok(b1) = e.cpu.read_u8(a.wrapping_add(i as u32)) else {
                    return Outcome::Fail("CRT string unmapped".into());
                };
                let Ok(b2) = e.cpu.read_u8(b.wrapping_add(i as u32)) else {
                    return Outcome::Fail("CRT string unmapped".into());
                };
                if b1 != b2 {
                    diff = (b1 as i32) - (b2 as i32);
                    break;
                }
                if b1 == 0 {
                    break;
                }
            }
            match diff.cmp(&0) {
                std::cmp::Ordering::Less => u32::MAX,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }
        }
        "strchr" => {
            let target = b as u8;
            let mut ptr = a;
            loop {
                let Ok(byte) = e.cpu.read_u8(ptr) else {
                    return Outcome::Fail("CRT string unmapped".into());
                };
                if byte == target {
                    return Outcome::Return(ptr);
                }
                if byte == 0 {
                    return Outcome::Return(0);
                }
                ptr = ptr.wrapping_add(1);
            }
        }
        "strrchr" => {
            let target = b as u8;
            let mut ptr = a;
            let mut last = 0;
            loop {
                let Ok(byte) = e.cpu.read_u8(ptr) else {
                    return Outcome::Fail("CRT string unmapped".into());
                };
                if byte == target {
                    last = ptr;
                }
                if byte == 0 {
                    return Outcome::Return(last);
                }
                ptr = ptr.wrapping_add(1);
            }
        }
        "strcpy" => {
            let mut d = a;
            let mut s = b;
            loop {
                let Ok(ch) = e.cpu.read_u8(s) else {
                    return Outcome::Fail("CRT strcpy unmapped".into());
                };
                let _ = e.cpu.write(d, &[ch]);
                if ch == 0 {
                    break;
                }
                d = d.wrapping_add(1);
                s = s.wrapping_add(1);
            }
            a
        }
        "strncpy" => {
            let n = c as usize;
            let mut null_hit = false;
            for i in 0..n {
                let byte = if null_hit {
                    0
                } else {
                    let Ok(val) = e.cpu.read_u8(b.wrapping_add(i as u32)) else {
                        return Outcome::Fail("CRT strncpy unmapped".into());
                    };
                    if val == 0 {
                        null_hit = true;
                    }
                    val
                };
                let _ = e.cpu.write(a.wrapping_add(i as u32), &[byte]);
            }
            a
        }
        "strcat" => {
            let mut d = a;
            loop {
                let Ok(ch) = e.cpu.read_u8(d) else {
                    return Outcome::Fail("CRT strcat dest unmapped".into());
                };
                if ch == 0 {
                    break;
                }
                d = d.wrapping_add(1);
            }
            let mut s = b;
            loop {
                let Ok(ch) = e.cpu.read_u8(s) else {
                    return Outcome::Fail("CRT strcat src unmapped".into());
                };
                let _ = e.cpu.write(d, &[ch]);
                if ch == 0 {
                    break;
                }
                d = d.wrapping_add(1);
                s = s.wrapping_add(1);
            }
            a
        }
        "strncat" => {
            let n = c as usize;
            let mut d = a;
            loop {
                let Ok(ch) = e.cpu.read_u8(d) else {
                    return Outcome::Fail("CRT strncat dest unmapped".into());
                };
                if ch == 0 {
                    break;
                }
                d = d.wrapping_add(1);
            }
            let mut s = b;
            let mut copied = 0;
            while copied < n {
                let Ok(ch) = e.cpu.read_u8(s) else {
                    return Outcome::Fail("CRT strncat src unmapped".into());
                };
                if ch == 0 {
                    break;
                }
                let _ = e.cpu.write(d, &[ch]);
                d = d.wrapping_add(1);
                s = s.wrapping_add(1);
                copied += 1;
            }
            let _ = e.cpu.write(d, &[0]);
            a
        }
        "wcslen" => {
            let mut ptr = a;
            let mut len = 0u32;
            loop {
                let Ok(unit) = e.cpu.read_u16(ptr) else {
                    return Outcome::Fail("CRT wcslen unmapped".into());
                };
                if unit == 0 {
                    break;
                }
                len += 1;
                ptr = ptr.wrapping_add(2);
            }
            len
        }
        "wcscmp" => {
            let mut p1 = a;
            let mut p2 = b;
            loop {
                let (Ok(u1), Ok(u2)) = (e.cpu.read_u16(p1), e.cpu.read_u16(p2)) else {
                    return Outcome::Fail("CRT wcscmp unmapped".into());
                };
                if u1 != u2 {
                    return match u1.cmp(&u2) {
                        std::cmp::Ordering::Less => Outcome::Return(u32::MAX),
                        std::cmp::Ordering::Equal => Outcome::Return(0),
                        std::cmp::Ordering::Greater => Outcome::Return(1),
                    };
                }
                if u1 == 0 {
                    return Outcome::Return(0);
                }
                p1 = p1.wrapping_add(2);
                p2 = p2.wrapping_add(2);
            }
        }
        "wcsncmp" => {
            let n = c as usize;
            if n == 0 {
                return Outcome::Return(0);
            }
            let mut p1 = a;
            let mut p2 = b;
            for _ in 0..n {
                let (Ok(u1), Ok(u2)) = (e.cpu.read_u16(p1), e.cpu.read_u16(p2)) else {
                    return Outcome::Fail("CRT wcsncmp unmapped".into());
                };
                if u1 != u2 {
                    return match u1.cmp(&u2) {
                        std::cmp::Ordering::Less => Outcome::Return(u32::MAX),
                        std::cmp::Ordering::Equal => Outcome::Return(0),
                        std::cmp::Ordering::Greater => Outcome::Return(1),
                    };
                }
                if u1 == 0 {
                    return Outcome::Return(0);
                }
                p1 = p1.wrapping_add(2);
                p2 = p2.wrapping_add(2);
            }
            0
        }
        "wcschr" => {
            let target = b as u16;
            let mut ptr = a;
            loop {
                let Ok(unit) = e.cpu.read_u16(ptr) else {
                    return Outcome::Fail("CRT wcschr unmapped".into());
                };
                if unit == target {
                    return Outcome::Return(ptr);
                }
                if unit == 0 {
                    return Outcome::Return(0);
                }
                ptr = ptr.wrapping_add(2);
            }
        }
        "wcsrchr" => {
            let target = b as u16;
            let mut ptr = a;
            let mut last = 0;
            loop {
                let Ok(unit) = e.cpu.read_u16(ptr) else {
                    return Outcome::Fail("CRT wcsrchr unmapped".into());
                };
                if unit == target {
                    last = ptr;
                }
                if unit == 0 {
                    return Outcome::Return(last);
                }
                ptr = ptr.wrapping_add(2);
            }
        }
        "wcscpy" => {
            let mut d = a;
            let mut s = b;
            loop {
                let Ok(ch) = e.cpu.read_u16(s) else {
                    return Outcome::Fail("CRT wcscpy unmapped".into());
                };
                let _ = e.cpu.write(d, &ch.to_le_bytes());
                if ch == 0 {
                    break;
                }
                d = d.wrapping_add(2);
                s = s.wrapping_add(2);
            }
            a
        }
        "wcsncpy" => {
            let n = c as usize;
            let mut d = a;
            let mut s = b;
            let mut null_seen = false;
            for _ in 0..n {
                let ch = if null_seen {
                    0
                } else {
                    let Ok(val) = e.cpu.read_u16(s) else {
                        return Outcome::Fail("CRT wcsncpy unmapped".into());
                    };
                    if val == 0 {
                        null_seen = true;
                    } else {
                        s = s.wrapping_add(2);
                    }
                    val
                };
                let _ = e.cpu.write(d, &ch.to_le_bytes());
                d = d.wrapping_add(2);
            }
            a
        }
        "wcscat" => {
            let mut d = a;
            loop {
                let Ok(ch) = e.cpu.read_u16(d) else {
                    return Outcome::Fail("CRT wcscat dest unmapped".into());
                };
                if ch == 0 {
                    break;
                }
                d = d.wrapping_add(2);
            }
            let mut s = b;
            loop {
                let Ok(ch) = e.cpu.read_u16(s) else {
                    return Outcome::Fail("CRT wcscat src unmapped".into());
                };
                let _ = e.cpu.write(d, &ch.to_le_bytes());
                if ch == 0 {
                    break;
                }
                d = d.wrapping_add(2);
                s = s.wrapping_add(2);
            }
            a
        }
        "wcsstr" => {
            if b == 0 {
                return Outcome::Return(a);
            }
            let Ok(first_needle) = e.cpu.read_u16(b) else {
                return Outcome::Fail("CRT wcsstr unmapped".into());
            };
            if first_needle == 0 {
                return Outcome::Return(a);
            }
            let mut hay = a;
            loop {
                let Ok(h_unit) = e.cpu.read_u16(hay) else {
                    return Outcome::Fail("CRT wcsstr unmapped".into());
                };
                if h_unit == 0 {
                    return Outcome::Return(0);
                }
                if h_unit == first_needle {
                    let mut h_sub = hay.wrapping_add(2);
                    let mut n_sub = b.wrapping_add(2);
                    let found = loop {
                        let Ok(n_val) = e.cpu.read_u16(n_sub) else {
                            return Outcome::Fail("CRT wcsstr unmapped".into());
                        };
                        if n_val == 0 {
                            break true;
                        }
                        let Ok(h_val) = e.cpu.read_u16(h_sub) else {
                            return Outcome::Fail("CRT wcsstr unmapped".into());
                        };
                        if h_val != n_val {
                            break false;
                        }
                        h_sub = h_sub.wrapping_add(2);
                        n_sub = n_sub.wrapping_add(2);
                    };
                    if found {
                        return Outcome::Return(hay);
                    }
                }
                hay = hay.wrapping_add(2);
            }
        }
        "_wcsicmp" => {
            let mut p1 = a;
            let mut p2 = b;
            loop {
                let (Ok(u1), Ok(u2)) = (e.cpu.read_u16(p1), e.cpu.read_u16(p2)) else {
                    return Outcome::Fail("CRT _wcsicmp unmapped".into());
                };
                let c1 = if u1 <= 127 { (u1 as u8).to_ascii_lowercase() as u16 } else { u1 };
                let c2 = if u2 <= 127 { (u2 as u8).to_ascii_lowercase() as u16 } else { u2 };
                if c1 != c2 {
                    return match c1.cmp(&c2) {
                        std::cmp::Ordering::Less => Outcome::Return(u32::MAX),
                        std::cmp::Ordering::Equal => Outcome::Return(0),
                        std::cmp::Ordering::Greater => Outcome::Return(1),
                    };
                }
                if u1 == 0 {
                    return Outcome::Return(0);
                }
                p1 = p1.wrapping_add(2);
                p2 = p2.wrapping_add(2);
            }
        }
        "_wcsnicmp" => {
            let n = c as usize;
            if n == 0 {
                return Outcome::Return(0);
            }
            let mut p1 = a;
            let mut p2 = b;
            for _ in 0..n {
                let (Ok(u1), Ok(u2)) = (e.cpu.read_u16(p1), e.cpu.read_u16(p2)) else {
                    return Outcome::Fail("CRT _wcsnicmp unmapped".into());
                };
                let c1 = if u1 <= 127 { (u1 as u8).to_ascii_lowercase() as u16 } else { u1 };
                let c2 = if u2 <= 127 { (u2 as u8).to_ascii_lowercase() as u16 } else { u2 };
                if c1 != c2 {
                    return match c1.cmp(&c2) {
                        std::cmp::Ordering::Less => Outcome::Return(u32::MAX),
                        std::cmp::Ordering::Equal => Outcome::Return(0),
                        std::cmp::Ordering::Greater => Outcome::Return(1),
                    };
                }
                if u1 == 0 {
                    return Outcome::Return(0);
                }
                p1 = p1.wrapping_add(2);
                p2 = p2.wrapping_add(2);
            }
            0
        }
        "_wfopen_s" => {
            let pfile = a;
            let filename_ptr = b;
            let mode_ptr = c;
            let mut name_u16 = Vec::new();
            let mut p = filename_ptr;
            while let Ok(u) = e.cpu.read_u16(p) {
                if u == 0 { break; }
                name_u16.push(u);
                p += 2;
            }
            let name_str = String::from_utf16_lossy(&name_u16);
            let mut mode_u16 = Vec::new();
            let mut p = mode_ptr;
            while let Ok(u) = e.cpu.read_u16(p) {
                if u == 0 { break; }
                mode_u16.push(u);
                p += 2;
            }
            let mode_str = String::from_utf16_lossy(&mode_u16);
            if let Some(stream) = crt_open_file(e, &name_str, &mode_str) {
                if pfile != 0 {
                    let _ = e.cpu.write(pfile, &stream.to_le_bytes());
                }
                0
            } else {
                if pfile != 0 {
                    let _ = e.cpu.write(pfile, &[0, 0, 0, 0]);
                }
                2 // ENOENT
            }
        }
        "_wfopen" => {
            let filename_ptr = a;
            let mode_ptr = b;
            let mut name_u16 = Vec::new();
            let mut p = filename_ptr;
            while let Ok(u) = e.cpu.read_u16(p) {
                if u == 0 { break; }
                name_u16.push(u);
                p += 2;
            }
            let name_str = String::from_utf16_lossy(&name_u16);
            let mut mode_u16 = Vec::new();
            let mut p = mode_ptr;
            while let Ok(u) = e.cpu.read_u16(p) {
                if u == 0 { break; }
                mode_u16.push(u);
                p += 2;
            }
            let mode_str = String::from_utf16_lossy(&mode_u16);
            crt_open_file(e, &name_str, &mode_str).unwrap_or(0)
        }
        "fopen_s" => {
            let pfile = a;
            let (Ok(name_bytes), Ok(mode_bytes)) = (e.cpu.read_cstr(b, 4096), e.cpu.read_cstr(c, 32)) else {
                return Outcome::Fail("CRT fopen_s string unmapped".into());
            };
            let name_str = String::from_utf8_lossy(&name_bytes);
            let mode_str = String::from_utf8_lossy(&mode_bytes);
            if let Some(stream) = crt_open_file(e, &name_str, &mode_str) {
                if pfile != 0 {
                    let _ = e.cpu.write(pfile, &stream.to_le_bytes());
                }
                0
            } else {
                if pfile != 0 {
                    let _ = e.cpu.write(pfile, &[0, 0, 0, 0]);
                }
                2 // ENOENT
            }
        }
        "fopen" => {
            let (Ok(name_bytes), Ok(mode_bytes)) = (e.cpu.read_cstr(a, 4096), e.cpu.read_cstr(b, 32)) else {
                return Outcome::Fail("CRT fopen string unmapped".into());
            };
            let name_str = String::from_utf8_lossy(&name_bytes);
            let mode_str = String::from_utf8_lossy(&mode_bytes);
            crt_open_file(e, &name_str, &mode_str).unwrap_or(0)
        }
        "fread" => {
            let buffer = a;
            let size = b as usize;
            let count = c as usize;
            let stream = e.arg(3);
            let total = size.saturating_mul(count);
            if total == 0 || size == 0 {
                return Outcome::Return(0);
            }
            let Some(state) = e.ext.try_get_mut::<CrtState>() else {
                return Outcome::Return(0);
            };
            let Some(file) = state.files.get_mut(&stream) else {
                return Outcome::Return(0);
            };
            use std::io::Read;
            let mut chunk = vec![0u8; total.min(16 * 1024 * 1024)];
            let bytes_read = file.read(&mut chunk).unwrap_or(0);
            if bytes_read > 0 {
                let _ = e.cpu.write(buffer, &chunk[..bytes_read]);
            }
            (bytes_read / size) as u32
        }
        "fwrite" => {
            let buffer = a;
            let size = b as usize;
            let count = c as usize;
            let stream = e.arg(3);
            let total = size.saturating_mul(count);
            if total == 0 || size == 0 {
                return Outcome::Return(0);
            }
            let mut chunk = vec![0u8; total];
            if e.cpu.read(buffer, &mut chunk).is_err() {
                return Outcome::Return(0);
            }
            let Some(state) = e.ext.try_get_mut::<CrtState>() else {
                return Outcome::Return(0);
            };
            let Some(file) = state.files.get_mut(&stream) else {
                return Outcome::Return(0);
            };
            use std::io::Write;
            let written = file.write(&chunk).unwrap_or(0);
            (written / size) as u32
        }
        "fseek" => {
            let stream = a;
            let offset = b as i32;
            let origin = c;
            let Some(state) = e.ext.try_get_mut::<CrtState>() else {
                return Outcome::Return(u32::MAX);
            };
            let Some(file) = state.files.get_mut(&stream) else {
                return Outcome::Return(u32::MAX);
            };
            use std::io::Seek;
            let seek_from = match origin {
                0 => std::io::SeekFrom::Start(offset as u64),
                1 => std::io::SeekFrom::Current(offset as i64),
                2 => std::io::SeekFrom::End(offset as i64),
                _ => return Outcome::Return(u32::MAX),
            };
            match file.seek(seek_from) {
                Ok(_) => 0,
                Err(_) => u32::MAX,
            }
        }
        "ftell" => {
            let stream = a;
            let Some(state) = e.ext.try_get_mut::<CrtState>() else {
                return Outcome::Return(u32::MAX);
            };
            let Some(file) = state.files.get_mut(&stream) else {
                return Outcome::Return(u32::MAX);
            };
            use std::io::Seek;
            match file.stream_position() {
                Ok(pos) => pos as u32,
                Err(_) => u32::MAX,
            }
        }
        "fclose" => {
            let stream = a;
            if stream != 0 {
                if let Some(state) = e.ext.try_get_mut::<CrtState>() {
                    state.files.remove(&stream);
                }
                let heaps = e.ext.get_mut::<Heaps>();
                heaps.free(e.cpu, &mut e.sys.vm, heaps.process_heap, stream);
            }
            0
        }
        "fflush" => {
            let stream = a;
            if let Some(state) = e.ext.try_get_mut::<CrtState>() {
                if let Some(file) = state.files.get_mut(&stream) {
                    use std::io::Write;
                    let _ = file.flush();
                }
            }
            0
        }
        "feof" => {
            let stream = a;
            let Some(state) = e.ext.try_get_mut::<CrtState>() else {
                return Outcome::Return(1);
            };
            let Some(file) = state.files.get_mut(&stream) else {
                return Outcome::Return(1);
            };
            use std::io::Seek;
            if let (Ok(cur), Ok(len)) = (file.stream_position(), file.metadata().map(|m| m.len())) {
                if cur >= len { 1 } else { 0 }
            } else {
                0
            }
        }
        "strncpy_s" => {
            let dest = a;
            let dest_size = b as usize;
            let src = c;
            let count = e.arg(3) as usize;
            if dest == 0 || dest_size == 0 {
                return Outcome::Return(22); // EINVAL
            }
            if src == 0 {
                let _ = e.cpu.write(dest, &[0]);
                return Outcome::Return(22);
            }
            if count == 0 {
                let _ = e.cpu.write(dest, &[0]);
                return Outcome::Return(0);
            }
            let truncate = count == usize::MAX || count == 0xffff_ffff;
            let Ok(src_str) = e.cpu.read_cstr(src, 16 * 1024 * 1024) else {
                return Outcome::Fail("CRT strncpy_s src unmapped".into());
            };
            let copy_len = if truncate {
                src_str.len().min(dest_size.saturating_sub(1))
            } else {
                src_str.len().min(count)
            };
            if !truncate && copy_len >= dest_size {
                let _ = e.cpu.write(dest, &[0]);
                return Outcome::Return(34); // ERANGE
            }
            if copy_len > 0 {
                let _ = e.cpu.write(dest, &src_str[..copy_len]);
            }
            let _ = e.cpu.write(dest.wrapping_add(copy_len as u32), &[0]);
            0
        }
        "strcpy_s" => {
            let dest = a;
            let dest_size = b as usize;
            let src = c;
            if dest == 0 || dest_size == 0 {
                return Outcome::Return(22);
            }
            if src == 0 {
                let _ = e.cpu.write(dest, &[0]);
                return Outcome::Return(22);
            }
            let Ok(src_str) = e.cpu.read_cstr(src, 16 * 1024 * 1024) else {
                return Outcome::Fail("CRT strcpy_s src unmapped".into());
            };
            if src_str.len() >= dest_size {
                let _ = e.cpu.write(dest, &[0]);
                return Outcome::Return(34);
            }
            let _ = e.cpu.write(dest, &src_str);
            let _ = e.cpu.write(dest.wrapping_add(src_str.len() as u32), &[0]);
            0
        }
        "strcat_s" => {
            let dest = a;
            let dest_size = b as usize;
            let src = c;
            if dest == 0 || dest_size == 0 {
                return Outcome::Return(22);
            }
            if src == 0 {
                let _ = e.cpu.write(dest, &[0]);
                return Outcome::Return(22);
            }
            let Ok(dest_str) = e.cpu.read_cstr(dest, dest_size) else {
                return Outcome::Fail("CRT strcat_s dest unmapped".into());
            };
            let Ok(src_str) = e.cpu.read_cstr(src, 16 * 1024 * 1024) else {
                return Outcome::Fail("CRT strcat_s src unmapped".into());
            };
            if dest_str.len() + src_str.len() >= dest_size {
                let _ = e.cpu.write(dest, &[0]);
                return Outcome::Return(34);
            }
            let _ = e.cpu.write(dest.wrapping_add(dest_str.len() as u32), &src_str);
            let _ = e.cpu.write(dest.wrapping_add((dest_str.len() + src_str.len()) as u32), &[0]);
            0
        }
        "strncat_s" => {
            let dest = a;
            let dest_size = b as usize;
            let src = c;
            let count = e.arg(3) as usize;
            if dest == 0 || dest_size == 0 {
                return Outcome::Return(22);
            }
            if src == 0 {
                let _ = e.cpu.write(dest, &[0]);
                return Outcome::Return(22);
            }
            if count == 0 {
                let _ = e.cpu.write(dest, &[0]);
                return Outcome::Return(0);
            }
            let Ok(dest_str) = e.cpu.read_cstr(dest, dest_size) else {
                return Outcome::Fail("CRT strncat_s dest unmapped".into());
            };
            let Ok(src_str) = e.cpu.read_cstr(src, 16 * 1024 * 1024) else {
                return Outcome::Fail("CRT strncat_s src unmapped".into());
            };
            let truncate = count == usize::MAX || count == 0xffff_ffff;
            let avail = dest_size.saturating_sub(dest_str.len()).saturating_sub(1);
            let append_len = if truncate {
                src_str.len().min(avail)
            } else {
                src_str.len().min(count)
            };
            if !truncate && append_len > avail {
                let _ = e.cpu.write(dest, &[0]);
                return Outcome::Return(34);
            }
            if append_len > 0 {
                let _ = e.cpu.write(dest.wrapping_add(dest_str.len() as u32), &src_str[..append_len]);
            }
            let _ = e.cpu.write(dest.wrapping_add((dest_str.len() + append_len) as u32), &[0]);
            0
        }
        "wcsncpy_s" => {
            let dest = a;
            let dest_size = b as usize;
            let src = c;
            let count = e.arg(3) as usize;
            if dest == 0 || dest_size == 0 {
                return Outcome::Return(22);
            }
            if src == 0 {
                let _ = e.cpu.write(dest, &[0, 0]);
                return Outcome::Return(22);
            }
            if count == 0 {
                let _ = e.cpu.write(dest, &[0, 0]);
                return Outcome::Return(0);
            }
            let mut src_units = Vec::new();
            let mut p = src;
            while let Ok(u) = e.cpu.read_u16(p) {
                if u == 0 { break; }
                src_units.push(u);
                p += 2;
            }
            let truncate = count == usize::MAX || count == 0xffff_ffff;
            let copy_len = if truncate {
                src_units.len().min(dest_size.saturating_sub(1))
            } else {
                src_units.len().min(count)
            };
            if !truncate && copy_len >= dest_size {
                let _ = e.cpu.write(dest, &[0, 0]);
                return Outcome::Return(34);
            }
            for i in 0..copy_len {
                let _ = e.cpu.write(dest.wrapping_add((i * 2) as u32), &src_units[i].to_le_bytes());
            }
            let _ = e.cpu.write(dest.wrapping_add((copy_len * 2) as u32), &[0, 0]);
            0
        }
        "wcscpy_s" => {
            let dest = a;
            let dest_size = b as usize;
            let src = c;
            if dest == 0 || dest_size == 0 {
                return Outcome::Return(22);
            }
            if src == 0 {
                let _ = e.cpu.write(dest, &[0, 0]);
                return Outcome::Return(22);
            }
            let mut src_units = Vec::new();
            let mut p = src;
            while let Ok(u) = e.cpu.read_u16(p) {
                if u == 0 { break; }
                src_units.push(u);
                p += 2;
            }
            if src_units.len() >= dest_size {
                let _ = e.cpu.write(dest, &[0, 0]);
                return Outcome::Return(34);
            }
            for i in 0..src_units.len() {
                let _ = e.cpu.write(dest.wrapping_add((i * 2) as u32), &src_units[i].to_le_bytes());
            }
            let _ = e.cpu.write(dest.wrapping_add((src_units.len() * 2) as u32), &[0, 0]);
            0
        }
        "wcscat_s" => {
            let dest = a;
            let dest_size = b as usize;
            let src = c;
            if dest == 0 || dest_size == 0 {
                return Outcome::Return(22);
            }
            if src == 0 {
                let _ = e.cpu.write(dest, &[0, 0]);
                return Outcome::Return(22);
            }
            let mut dest_len = 0usize;
            let mut p = dest;
            while let Ok(u) = e.cpu.read_u16(p) {
                if u == 0 { break; }
                dest_len += 1;
                p += 2;
            }
            let mut src_units = Vec::new();
            let mut p = src;
            while let Ok(u) = e.cpu.read_u16(p) {
                if u == 0 { break; }
                src_units.push(u);
                p += 2;
            }
            if dest_len + src_units.len() >= dest_size {
                let _ = e.cpu.write(dest, &[0, 0]);
                return Outcome::Return(34);
            }
            for i in 0..src_units.len() {
                let _ = e.cpu.write(dest.wrapping_add(((dest_len + i) * 2) as u32), &src_units[i].to_le_bytes());
            }
            let _ = e.cpu.write(dest.wrapping_add(((dest_len + src_units.len()) * 2) as u32), &[0, 0]);
            0
        }
        "strstr" => {
            let Ok(needle) = e.cpu.read_cstr(b, 65536) else {
                return Outcome::Fail("CRT needle unmapped".into());
            };
            if needle.is_empty() {
                return Outcome::Return(a);
            }
            let Ok(haystack) = e.cpu.read_cstr(a, 16 * 1024 * 1024) else {
                return Outcome::Fail("CRT haystack unmapped".into());
            };
            if let Some(pos) = haystack.windows(needle.len()).position(|w| w == needle) {
                a.wrapping_add(pos as u32)
            } else {
                0
            }
        }
        "_strdup" => {
            if a == 0 {
                return Outcome::Return(0);
            }
            let Ok(s) = e.cpu.read_cstr(a, 16 * 1024 * 1024) else {
                return Outcome::Fail("CRT string unmapped".into());
            };
            let len = (s.len() + 1) as u32;
            let ptr = allocate(e, len, false);
            if ptr != 0 {
                let _ = e.cpu.write(ptr, &s);
                let _ = e.cpu.write(ptr.wrapping_add(s.len() as u32), &[0]);
            }
            ptr
        }
        "strtoul" => {
            let Ok(s) = e.cpu.read_cstr(a, 1024) else {
                return Outcome::Fail("CRT string unmapped".into());
            };
            let wide: Vec<u16> = s.iter().map(|&byte| byte as u16).collect();
            let (val, consumed, _) = convert::parse(&wide, c as i32);
            if b != 0 {
                let end = a.wrapping_add(consumed as u32);
                let _ = e.cpu.write_u32(b, end);
            }
            val as u32
        }
        "_mbsinc" => {
            let Ok(b1) = e.cpu.read_u8(a) else {
                return Outcome::Fail("CRT string unmapped".into());
            };
            if (0x81..=0x9f).contains(&b1) || (0xe0..=0xfc).contains(&b1) {
                a.wrapping_add(2)
            } else {
                a.wrapping_add(1)
            }
        }
        "_ismbblead" => {
            let acp = e.ext.get::<crate::Win32>().codec.acp;
            let byte = a as u8;
            let is_lead = a <= 0xff
                && match acp {
                    936 => (0x81..=0xfe).contains(&byte),
                    _ => (0x81..=0x9f).contains(&byte) || (0xe0..=0xfc).contains(&byte),
                };
            if is_lead {
                1
            } else {
                0
            }
        }
        "_ismbbtrail" => {
            let acp = e.ext.get::<crate::Win32>().codec.acp;
            let byte = a as u8;
            let is_trail = a <= 0xff
                && match acp {
                    936 => (0x40..=0x7e).contains(&byte) || (0x80..=0xfe).contains(&byte),
                    _ => (0x40..=0x7e).contains(&byte) || (0x80..=0xfc).contains(&byte),
                };
            if is_trail {
                1
            } else {
                0
            }
        }
        "_mbsnbcpy" => {
            let n = c as usize;
            let mut null_hit = false;
            for i in 0..n {
                let byte = if null_hit {
                    0
                } else {
                    let Ok(val) = e.cpu.read_u8(b.wrapping_add(i as u32)) else {
                        return Outcome::Fail("CRT string unmapped".into());
                    };
                    if val == 0 {
                        null_hit = true;
                    }
                    val
                };
                if let Err(err) = e.cpu.write(a.wrapping_add(i as u32), &[byte]) {
                    return Outcome::Fail(err.to_string());
                }
            }
            a
        }
        "_splitpath" => {
            let Ok(path) = e.cpu.read_cstr(a, 4096) else {
                return Outcome::Fail("CRT path unmapped".into());
            };
            let (drive, dir, fname, ext) = split_path_a(&path);
            let ptrs = [b, c, e.arg(3), e.arg(4)];
            let parts = [drive, dir, fname, ext];
            for (p, part) in ptrs.into_iter().zip(parts) {
                if p != 0 {
                    let _ = e.cpu.write(p, part);
                    let _ = e.cpu.write(p.wrapping_add(part.len() as u32), &[0]);
                }
            }
            0
        }
        "_setjmp3" => {
            let jmp_buf = a;
            let count = b;
            if jmp_buf != 0 {
                let teb = e.ctx().teb;
                let exception_list = e.cpu.read_u32(teb).unwrap_or(u32::MAX);
                let esp = e.cpu.reg(Reg::Esp);
                let caller = e.caller();

                let _ = e.cpu.write_u32(jmp_buf, e.cpu.reg(Reg::Ebp));
                let _ = e.cpu.write_u32(jmp_buf + 4, e.cpu.reg(Reg::Ebx));
                let _ = e.cpu.write_u32(jmp_buf + 8, e.cpu.reg(Reg::Edi));
                let _ = e.cpu.write_u32(jmp_buf + 12, e.cpu.reg(Reg::Esi));
                let _ = e.cpu.write_u32(jmp_buf + 16, esp.wrapping_add(4));
                let _ = e.cpu.write_u32(jmp_buf + 20, caller);
                let _ = e.cpu.write_u32(jmp_buf + 24, exception_list);
                let _ = e.cpu.write_u32(jmp_buf + 28, 0);

                for i in 0..count.min(6) {
                    let val = e.arg(2 + i);
                    let _ = e.cpu.write_u32(jmp_buf + 32 + i * 4, val);
                }
            }
            0
        }
        "getenv" => 0,
        "_makepath" => {
            let mut out = Vec::new();
            if b != 0 && let Ok(drive) = e.cpu.read_cstr(b, 260) {
                out.extend_from_slice(&drive);
                if drive.len() == 1 && drive[0] != b':' {
                    out.push(b':');
                }
            }
            if c != 0 && let Ok(dir) = e.cpu.read_cstr(c, 260) {
                out.extend_from_slice(&dir);
                if !dir.is_empty() && dir.last() != Some(&b'/') && dir.last() != Some(&b'\\') {
                    out.push(b'\\');
                }
            }
            let d = e.arg(3);
            if d != 0 && let Ok(fname) = e.cpu.read_cstr(d, 260) {
                out.extend_from_slice(&fname);
            }
            let ext_ptr = e.arg(4);
            if ext_ptr != 0 && let Ok(ext) = e.cpu.read_cstr(ext_ptr, 260) {
                if !ext.is_empty() && ext[0] != b'.' {
                    out.push(b'.');
                }
                out.extend_from_slice(&ext);
            }
            out.push(0);
            if let Err(err) = e.cpu.write(a, &out) {
                return Outcome::Fail(err.to_string());
            }
            0
        }
        "_beginthread" => {
            let start = a;
            let stack = b;
            let arg = c;
            let name = format!("CrtT{}", e.sys.contexts.len());
            match crate::kernel32::spawn_thread(
                e,
                &name,
                start,
                arg,
                if stack == 0 { 0x10_0000 } else { stack },
            ) {
                Ok(id) => e.sys.context(id).thread_handle,
                Err(_) => u32::MAX,
            }
        }
        "_XcptFilter" => 1,
        "__setusermatherr" => 0,
        "floor" | "ceil" | "ldexp" | "frexp" => {
            let x = match double(e) {
                Ok(v) => v,
                Err(err) => return Outcome::Fail(err),
            };
            let result = match name.as_str() {
                "floor" => x.floor(),
                "ceil" => x.ceil(),
                "ldexp" => scale(x, c as i32),
                _ => {
                    let (v, exp) = split_fraction(x);
                    if let Err(err) = e.cpu.write_u32(c, exp as u32) {
                        return Outcome::Fail(err.to_string());
                    }
                    v
                }
            };
            e.cpu.push_f64(result);
            0
        }
        "_CIpow" => {
            let (Some(y), Some(x)) = (e.cpu.pop_f64(), e.cpu.pop_f64()) else {
                return Outcome::Fail("_CIpow: empty x87 stack".into());
            };
            e.cpu.push_f64(x.powf(y));
            0
        }
        "_ftol" => {
            let Some(x) = e.cpu.pop_f64() else {
                return Outcome::Fail("_ftol: empty x87 stack".into());
            };
            let value = if !x.is_finite()
                || !(-9223372036854775808.0..9223372036854775808.0).contains(&x)
            {
                i64::MIN
            } else {
                x.trunc() as i64
            };
            return Outcome::Return64(value as u64);
        }
        _ => {
            return Outcome::Fail(format!(
                "unsupported CRT {name} at {:#x}",
                e.cpu.reg(Reg::Eip)
            ));
        }
    };
    Outcome::Return(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Guest, Prot};
    use adv_machine::{Clock, Machine, RunResult};
    use iced_x86::code_asm::*;
    fn machine(jit: bool) -> Machine<JitCpu> {
        let mut reg = Registry::default();
        crate::register(&mut reg);
        register(&mut reg);
        let mut m = Machine::new(
            JitCpu::new(adv_machine::layout::PARK_ADDR, jit).unwrap(),
            reg,
            Clock::virtual_time(),
        )
        .unwrap();
        crate::Win32::install(
            &mut m,
            crate::Win32Config::new(".", "crt-test.exe", "target/crt-test"),
        )
        .unwrap();
        install(&mut m).unwrap();
        m
    }
    #[test]
    fn ucrt_heap_apisets_share_allocations_and_preserve_contents_on_failed_growth() {
        for jit in [false, true] {
            let mut m = machine(jit);
            let code = 0x400000;
            let data = 0x500000;
            m.cpu.map(code, 4096, Prot::RX).unwrap();
            m.cpu.map(data, 4096, Prot::RW).unwrap();
            let heap = "api-ms-win-crt-heap-l1-1-0.dll";
            let malloc = m.resolve_import(heap, "malloc").unwrap();
            let calloc = m.resolve_import(heap, "calloc").unwrap();
            let realloc = m.resolve_import("ucrtbase.dll", "realloc").unwrap();
            let free = m.resolve_import("ucrtbase.dll", "free").unwrap();
            let errno = m.resolve_import("ucrtbase.dll", "_errno").unwrap();
            let mut a = CodeAssembler::new(32).unwrap();
            a.push(16).unwrap();
            a.call(u64::from(malloc)).unwrap();
            a.add(esp, 4).unwrap();
            a.mov(dword_ptr(data as i32), eax).unwrap();
            a.mov(ebx, eax).unwrap();
            a.mov(dword_ptr(ebx), 0x1234abcd).unwrap();
            a.push(64).unwrap();
            a.push(ebx).unwrap();
            a.call(u64::from(realloc)).unwrap();
            a.add(esp, 8).unwrap();
            a.mov(ebx, eax).unwrap();
            a.mov(dword_ptr((data + 4) as i32), ebx).unwrap();
            a.push(-1).unwrap();
            a.push(ebx).unwrap();
            a.call(u64::from(realloc)).unwrap();
            a.add(esp, 8).unwrap();
            a.mov(dword_ptr((data + 8) as i32), eax).unwrap();
            a.mov(eax, dword_ptr(ebx)).unwrap();
            a.mov(dword_ptr((data + 12) as i32), eax).unwrap();
            a.call(u64::from(errno)).unwrap();
            a.mov(eax, dword_ptr(eax)).unwrap();
            a.mov(dword_ptr((data + 16) as i32), eax).unwrap();
            a.push(ebx).unwrap();
            a.call(u64::from(free)).unwrap();
            a.add(esp, 4).unwrap();
            a.push(4).unwrap();
            a.push(8).unwrap();
            a.call(u64::from(calloc)).unwrap();
            a.add(esp, 8).unwrap();
            a.mov(ebx, eax).unwrap();
            a.mov(eax, dword_ptr(ebx + 28)).unwrap();
            a.mov(dword_ptr((data + 20) as i32), eax).unwrap();
            a.push(ebx).unwrap();
            a.call(u64::from(free)).unwrap();
            a.add(esp, 4).unwrap();
            a.push(4).unwrap();
            a.push(-1).unwrap();
            a.call(u64::from(calloc)).unwrap();
            a.add(esp, 8).unwrap();
            a.mov(dword_ptr((data + 24) as i32), eax).unwrap();
            a.xor(eax, eax).unwrap();
            a.ret().unwrap();
            m.cpu
                .write(code, &a.assemble(u64::from(code)).unwrap())
                .unwrap();
            m.spawn("ucrt-heap", code, &[], 65536).unwrap();
            assert!(matches!(m.run(), RunResult::Exited(0)));
            let original = m.cpu.read_u32(data).unwrap();
            let resized = m.cpu.read_u32(data + 4).unwrap();
            assert_ne!(original, 0);
            assert_ne!(resized, 0);
            assert!(m.ext.get::<Heaps>().owner(original).is_none());
            assert!(m.ext.get::<Heaps>().owner(resized).is_none());
            for (offset, expected) in [(8, 0), (12, 0x1234abcd), (16, 12), (20, 0), (24, 0)] {
                assert_eq!(m.cpu.read_u32(data + offset).unwrap(), expected);
            }
        }
    }
    #[test]
    fn qsort_calls_guest_comparator_and_initializer_callbacks_in_order() {
        for jit in [false, true] {
            let mut m = machine(jit);
            let code = 0x400000;
            let data = 0x500000;
            m.cpu.map(code, 4096, Prot::RX).unwrap();
            m.cpu.map(data, 4096, Prot::RW).unwrap();
            let sort = m.resolve_import("msvcrt.dll", "qsort").unwrap();
            let init = m.resolve_import("msvcrt.dll", "_initterm").unwrap();
            let mut a = CodeAssembler::new(32).unwrap();
            a.push(data + 8).unwrap();
            a.push(data).unwrap();
            a.call(u64::from(init)).unwrap();
            a.add(esp, 8).unwrap();
            a.push(code + 0x100).unwrap();
            a.push(4).unwrap();
            a.push(5).unwrap();
            a.push(data + 32).unwrap();
            a.call(u64::from(sort)).unwrap();
            a.add(esp, 16).unwrap();
            a.xor(eax, eax).unwrap();
            a.ret().unwrap();
            m.cpu
                .write(code, &a.assemble(u64::from(code)).unwrap())
                .unwrap();
            let mut a = CodeAssembler::new(32).unwrap();
            a.mov(eax, dword_ptr(esp + 4)).unwrap();
            a.mov(eax, dword_ptr(eax)).unwrap();
            a.mov(edx, dword_ptr(esp + 8)).unwrap();
            a.sub(eax, dword_ptr(edx)).unwrap();
            a.ret().unwrap();
            m.cpu
                .write(code + 0x100, &a.assemble(u64::from(code + 0x100)).unwrap())
                .unwrap();
            for (pc, mult, add) in [(code + 0x200, 1, 3), (code + 0x220, 2, 1)] {
                let mut a = CodeAssembler::new(32).unwrap();
                a.mov(eax, dword_ptr((data + 16) as i32)).unwrap();
                if mult == 2 {
                    a.add(eax, eax).unwrap();
                }
                a.add(eax, add).unwrap();
                a.mov(dword_ptr((data + 16) as i32), eax).unwrap();
                a.ret().unwrap();
                m.cpu
                    .write(pc, &a.assemble(u64::from(pc)).unwrap())
                    .unwrap();
            }
            m.cpu
                .write(
                    data,
                    &[code + 0x200, code + 0x220]
                        .into_iter()
                        .flat_map(u32::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            m.cpu
                .write(
                    data + 32,
                    &[7i32, -3, 0, 7, -9]
                        .into_iter()
                        .flat_map(i32::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            m.spawn("crt", code, &[], 65536).unwrap();
            assert!(matches!(m.run(), RunResult::Exited(0)));
            assert_eq!(m.cpu.read_u32(data + 16).unwrap(), 7);
            let mut sorted = [0; 20];
            m.cpu.read(data + 32, &mut sorted).unwrap();
            assert_eq!(
                sorted.to_vec(),
                [-9i32, -3, 0, 7, 7]
                    .into_iter()
                    .flat_map(i32::to_le_bytes)
                    .collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn crt_globals_store_app_type_and_invalid_ranges_or_lock_owners_fail_loudly() {
        for jit in [false, true] {
            let code = 0x400000;
            let data = 0x500000;
            let cases = [
                ("_initterm_e", vec![0xdead0000, 0xdead0004]),
                ("_initterm_e", vec![data, data + 3]),
                ("_lock", vec![64]),
                ("_unlock", vec![8]),
                ("__dllonexit", vec![code, data + 16, data + 20]),
            ];
            for (name, args) in cases {
                let mut m = machine(jit);
                m.cpu.map(code, 4096, Prot::RX).unwrap();
                m.cpu.map(data, 4096, Prot::RW).unwrap();
                m.cpu.write_u32(data + 16, data + 64).unwrap();
                m.cpu.write_u32(data + 20, data + 63).unwrap();
                let target = m.resolve_import("msvcr90.dll", name).unwrap();
                let mut a = CodeAssembler::new(32).unwrap();
                for value in args.iter().rev() {
                    a.push(*value).unwrap();
                }
                a.call(u64::from(target)).unwrap();
                a.add(esp, (args.len() * 4) as i32).unwrap();
                a.xor(eax, eax).unwrap();
                a.ret().unwrap();
                m.cpu
                    .write(code, &a.assemble(u64::from(code)).unwrap())
                    .unwrap();
                m.spawn("invalid-crt", code, &[], 65536).unwrap();
                assert!(matches!(m.run(), RunResult::Failed(_)), "{name} jit={jit}");
            }
            let mut m = machine(jit);
            m.cpu.map(code, 4096, Prot::RX).unwrap();
            let target = m.resolve_import("msvcr90.dll", "__set_app_type").unwrap();
            let mut a = CodeAssembler::new(32).unwrap();
            a.push(2).unwrap();
            a.call(u64::from(target)).unwrap();
            a.add(esp, 4).unwrap();
            a.xor(eax, eax).unwrap();
            a.ret().unwrap();
            m.cpu
                .write(code, &a.assemble(u64::from(code)).unwrap())
                .unwrap();
            m.spawn("crt-global", code, &[], 65536).unwrap();
            assert!(matches!(m.run(), RunResult::Exited(0)));
            let pointer =
                m.ext.get::<CrtState>().globals[&("msvcr90.dll".into(), "app_type".into())];
            assert_eq!(m.cpu.read_u32(pointer).unwrap(), 2);
        }
    }
    #[test]
    fn errno_cells_are_freed_at_thread_and_process_detach_boundaries() {
        for jit in [false, true] {
            let mut registry = Registry::default();
            crate::register(&mut registry);
            register(&mut registry);
            registry.add("fixture.dll", "cleanup", Conv::Cdecl { argc: 0 }, |env| {
                let heap = env.ext.get::<Heaps>().process_heap;
                let mut cells = Vec::new();
                for (module, ctx) in [
                    ("msvcrt.dll", env.context),
                    ("ucrtbase.dll", env.context),
                    ("msvcrt.dll", env.context + 1),
                ] {
                    let address = allocate(env, 4, true);
                    if address == 0 {
                        return Outcome::Fail("fixture allocation failed".into());
                    }
                    env.ext
                        .get_mut::<CrtState>()
                        .errno
                        .insert((module.into(), ctx), address);
                    cells.push(address);
                }
                if let Err(error) = detach_thread(env) {
                    return Outcome::Fail(error);
                }
                assert_eq!(env.ext.get::<CrtState>().errno.len(), 1);
                for &address in &cells[..2] {
                    assert_eq!(env.ext.get_mut::<Heaps>().size(heap, address), None);
                }
                assert_eq!(env.ext.get_mut::<Heaps>().size(heap, cells[2]), Some(4));
                // Repeated notification cleanup is idempotent.
                detach_thread(env).unwrap();
                detach_process(env).unwrap();
                detach_process(env).unwrap();
                assert!(env.ext.get::<CrtState>().errno.is_empty());
                assert_eq!(env.ext.get_mut::<Heaps>().size(heap, cells[2]), None);
                Outcome::Return(0)
            });
            let mut machine = Machine::new(
                JitCpu::new(adv_machine::layout::PARK_ADDR, jit).unwrap(),
                registry,
                Clock::virtual_time(),
            )
            .unwrap();
            crate::Win32::install(
                &mut machine,
                crate::Win32Config::new(".", "errno-test.exe", "target/errno-test"),
            )
            .unwrap();
            install(&mut machine).unwrap();
            let code = 0x400000;
            machine.cpu.map(code, 4096, Prot::RX).unwrap();
            let target = machine.resolve_import("fixture.dll", "cleanup").unwrap();
            let mut asm = adv_cpu::backend::asm::Asm::new(code);
            asm.call(target).ret();
            machine.cpu.write(code, &asm.bytes).unwrap();
            machine.spawn("errno-detach", code, &[], 65536).unwrap();
            assert!(matches!(machine.run(), RunResult::Exited(0)));
        }
    }

    #[test]
    fn frexp_ldexp_helpers_preserve_subnormal_sign_and_full_double_precision() {
        for v in [
            0.0,
            -0.0,
            1.0000000000000002,
            -12345.6789,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            f64::MAX,
        ] {
            let (fraction, exponent) = split_fraction(v);
            assert_eq!(scale(fraction, exponent).to_bits(), v.to_bits());
        }
    }

    #[test]
    fn setjmp3_is_registered_in_all_crt_variants() {
        let mut r = Registry::default();
        register(&mut r);
        assert!(r.get("msvcrt.dll", "_setjmp3").is_some());
        assert!(r.get("msvcr80.dll", "_setjmp3").is_some());
        assert!(r.get("msvcr90.dll", "_setjmp3").is_some());
    }

    #[test]
    fn vector_new_delete_is_registered_in_all_crt_variants() {
        let mut r = Registry::default();
        register(&mut r);
        for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
            assert!(r.get(module, "??_U@YAPAXI@Z").is_some());
            assert!(r.get(module, "??_V@YAXPAX@Z").is_some());
        }
    }
}

