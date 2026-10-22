//! Small C-runtime services used by guest library DLLs. Library algorithms remain guest code.
use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Reg};
use adv_machine::{Conv, Env, Outcome};
use adv_os::Heaps;
use std::collections::HashMap;
mod access;
mod byte_output;
mod byte_set;
mod byte_sort;
mod convert;
mod cpp_alloc;
pub(crate) mod cpp_eh;
mod cpp_runtime;
mod data;
mod directory;
mod errors;
mod exceptions;
mod file;
mod find;
mod formatting;
pub(crate) use data::initialize as initialize_data;
mod floating;
mod heap;
mod intrinsic_math;
mod locale;
mod memory_ops;
mod modern_stdio;
mod modern_string;
mod onexit;
mod path;
mod process;
mod random;
mod scalar_float;
mod scalar_log;
mod scalar_power;
mod scalar_round;
mod scalar_scale;
mod scalar_split;
mod scan;
mod secure_memory;
mod seh;
mod shared_file;
mod startup;
mod stream_position;
mod strings;
mod thread;
mod time64;
mod versions;
mod wide_scan;
mod wide_string;

#[derive(Default)]
struct CrtState {
    errno: HashMap<(String, u32), u32>,
    tm_buffers: HashMap<(String, u32), u32>,
    random_seeds: HashMap<(String, u32), u32>,
    locks: HashMap<(String, u32), u32>,
    globals: HashMap<(String, String), u32>,
    invalid_handlers: HashMap<String, u32>,
    files: HashMap<u32, CrtFile>,
    locales: HashMap<String, locale::LocaleState>,
    cpp_eh_threads: HashMap<(String, u32), cpp_eh::Thread>,
    cpp_threads: HashMap<(String, u32), cpp_runtime::Thread>,
    purecall_handlers: HashMap<String, u32>,
}

// The NT handle owns bytes; FILE retains read-ahead and its logical cursor.
struct CrtFile {
    handle: u32,
    binary: bool,
    append: bool,
    eof: bool,
    position: u64,
    read_buffer: [u8; 4096],
    read_cursor: usize,
    read_end: usize,
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

fn invalid_parameter_result(env: &mut Env<'_>, function: &str, error: i32, result: u32) -> Outcome {
    invalid_parameter_then(env, function, error, move |_| Outcome::Return(result))
}

fn invalid_parameter_then(
    env: &mut Env<'_>,
    function: &str,
    error: i32,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let errno = floating::errno_address(env);
    if errno.is_null() || errno.write(env.cpu, error).is_err() {
        return Outcome::Fail("CRT errno allocation/write failed".into());
    }
    let handler = invalid_handler(env);
    if handler == 0 {
        return Outcome::Fail(format!(
            "{function}: invalid parameter without a returning guest handler"
        ));
    }
    // Release UCRT passes no diagnostic strings. errno is assigned before the
    // callback, and a returning callback may change it or wait via the Machine.
    Outcome::call(handler, [0, 0, 0, 0, 0], move |env, _| then(env))
}

fn invalid_handler(env: &Env<'_>) -> u32 {
    env.ext
        .try_get::<CrtState>()
        .and_then(|state| {
            state
                .invalid_handlers
                .get(crt_module(&env.sys.thunks.get(env.thunk).module))
                .copied()
        })
        .unwrap_or(0)
}

// Inlined checks (such as checked STL iterators) report through this entry. A
// registered handler returns to the caller; without one, _invoke_watson ends the
// process with STATUS_INVALID_CRT_PARAMETER.
fn invalid_parameter_noinfo(env: &mut Env<'_>) -> Outcome {
    let handler = invalid_handler(env);
    if handler == 0 {
        return crate::kernel32::exit_process(env, 0xc000_0417);
    }
    Outcome::call(handler, [0, 0, 0, 0, 0], |_, _| Outcome::Return(0))
}

pub fn install<B: adv_cpu::backend::Backend>(
    machine: &mut adv_machine::Machine<B>,
) -> Result<(), String> {
    machine.ext.insert(CrtState::default());
    Ok(())
}

pub fn register(r: &mut Registry) {
    access::register(r);
    byte_set::register(r);
    byte_sort::register(r);
    byte_output::register(r);
    data::register(r);
    directory::register(r);
    exceptions::register(r);
    find::register(r);
    formatting::register(r);
    floating::register(r);
    errors::register(r);
    intrinsic_math::register(r);
    time64::register(r);
    random::register(r);
    scalar_float::register(r);
    scalar_log::register(r);
    scalar_power::register(r);
    scalar_round::register(r);
    scalar_scale::register(r);
    scalar_split::register(r);
    onexit::register(r);
    path::register(r);
    startup::register(r);
    convert::register(r);
    cpp_alloc::register(r);
    cpp_runtime::register(r);
    cpp_eh::register(r);
    scan::register(r);
    wide_scan::register(r);
    secure_memory::register(r);
    seh::register(r);
    shared_file::register(r);
    stream_position::register(r);
    thread::register(r);
    heap::register(r);
    locale::register(r);
    memory_ops::register(r);
    modern_stdio::register(r);
    modern_string::register(r);
    wide_string::register(r);
    process::register(r);
    strings::register(r);
    versions::register(
        r,
        &[("_setjmp", call), ("_setjmp3", call), ("longjmp", call)],
    );
    for (name, argc) in [
        ("malloc", 1),
        ("calloc", 2),
        ("realloc", 2),
        ("free", 1),
        ("memcpy", 3),
        ("memmove", 3),
        ("memset", 3),
        ("memcmp", 3),
        ("_initterm", 2),
        ("_initterm_e", 2),
        ("_lock", 1),
        ("_unlock", 1),
        ("__dllonexit", 3),
        ("__set_app_type", 1),
        ("__p__fmode", 0),
        ("__p__commode", 0),
        ("frexp", 3),
        ("_ftol", 0),
        ("time", 1),
        ("_time32", 1),
        ("gmtime", 1),
        ("localtime", 1),
        ("atoi", 1),
        ("abs", 1),
        ("_itoa", 3),
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
        ("??_U@YAPAXI@Z", 1),
        ("??_V@YAXPAX@Z", 1),
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
        // MinGW-built DLLs import these from the system msvcrt (WIN32-API-18).
        ("_stricmp", 2),
        ("_strcmpi", 2),
        ("_strnicmp", 3),
        ("__p__iob", 0),
        ("abort", 0),
        ("_amsg_exit", 1),
        ("_purecall", 0),
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
            ("_invalid_parameter_noinfo", 0),
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
            ("_time32", 1),
            ("_gmtime32", 1),
            ("_localtime32", 1),
            ("_ismbblead", 1),
            ("_ismbbtrail", 1),
            ("memcpy", 3),
            ("memmove", 3),
            ("memset", 3),
            ("memcmp", 3),
            ("_strdup", 1),
            ("strtoul", 3),
            ("_mbsinc", 1),
            ("_mbsnbcpy", 3),
            ("_splitpath", 5),
            ("_makepath", 5),
            ("getenv", 1),
            ("??_U@YAPAXI@Z", 1),
            ("??_V@YAXPAX@Z", 1),
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
    }
    file::leaves::register(r);
    crate::guest_code::install_crt(r);
}
/// Release thread-owned cells only after the existing DLL/TLS detach callbacks.
pub(crate) fn detach_thread(env: &mut Env<'_>) -> Result<(), String> {
    let context = env.context;
    if let Some(state) = env.ext.try_get_mut::<CrtState>() {
        state.cpp_eh_threads.retain(|(_, id), _| *id != context);
        state.cpp_threads.retain(|(_, id), _| *id != context);
    }
    random::release(env, Some(env.context));
    time64::release(env, Some(env.context))?;
    release_errno(env, Some(env.context))
}

/// Host CRT modules are pinned. Process detach is their final errno boundary.
pub(crate) fn detach_process(env: &mut Env<'_>) -> Result<(), String> {
    if let Some(state) = env.ext.try_get_mut::<CrtState>() {
        state.cpp_eh_threads.clear();
        state.cpp_threads.clear();
        state.purecall_handlers.clear();
    }
    random::release(env, None);
    onexit::release_process_tables(env)?;
    startup::release_process_tables(env)?;
    locale::release_process_tables(env)?;
    time64::release(env, None)?;
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
/// 'VC20', marking a `_setjmp3` buffer that carries an unwind function.
const JMP_MAGIC: u32 = 0x5643_3230;

/// MSVC x86 jump buffers retain the target registration and local unwind
/// callback. Global unwinding precedes the compiler-supplied local cleanup.
fn long_jump(e: &mut Env<'_>, buffer: u32, value: u32) -> Outcome {
    let mut words = [0; 10];
    for (index, word) in words.iter_mut().enumerate() {
        match e.cpu.read_u32(buffer.wrapping_add(4 * index as u32)) {
            Ok(read) => *word = read,
            Err(error) => return adv_abi::guest::memory_failure(error),
        }
    }
    let registration = words[6];
    let current = match e.cpu.read_u32(e.ctx().teb) {
        Ok(frame) => frame,
        Err(error) => return adv_abi::guest::memory_failure(error),
    };
    let value = if value == 0 { 1 } else { value };
    if registration != current {
        return seh::unwind_global(
            e,
            if registration == u32::MAX {
                0
            } else {
                registration
            },
            0,
            Box::new(move |e| jump_local(e, buffer, words, value)),
        );
    }
    jump_local(e, buffer, words, value)
}

fn jump_local(e: &mut Env<'_>, buffer: u32, words: [u32; 10], value: u32) -> Outcome {
    let [.., registration, try_level, cookie, unwind] = words;
    if registration == u32::MAX {
        return restore_jump(e, words, value);
    }
    if cookie == JMP_MAGIC && unwind != 0 {
        return Outcome::call(unwind, [buffer], move |e, _| restore_jump(e, words, value))
            .unwindable();
    }
    seh::unwind_to(
        e,
        registration,
        try_level as i32,
        Box::new(move |e| restore_jump(e, words, value)),
    )
}

fn restore_jump(e: &mut Env<'_>, words: [u32; 10], value: u32) -> Outcome {
    let [ebp, ebx, edi, esi, esp, eip, ..] = words;
    let mut checkpoint = match e.checkpoint_for_stack_unwind(esp) {
        Ok(checkpoint) => checkpoint,
        Err(reason) => return Outcome::Fail(format!("longjmp continuation: {reason}")),
    };
    let regs = &mut checkpoint.state.regs;
    (regs[0], regs[3], regs[4], regs[5], regs[6], regs[7]) = (value, ebx, esp, ebp, esi, edi);
    checkpoint.state.eip = eip;
    Outcome::ResumeExecution(checkpoint.unwind_callbacks())
}

fn call(e: &mut Env<'_>) -> Outcome {
    let name = e.sys.thunks.get(e.thunk).name.clone();
    let arg = |i| e.arg(i);
    let (a, b, c) = (arg(0), arg(1), arg(2));
    let value = match name.as_str() {
        "__set_app_type" | "__p__fmode" | "__p__commode" | "__iob_func" | "__p__iob" => {
            if e.ext.try_get::<CrtState>().is_none() {
                e.ext.insert(CrtState::default());
            }
            let key = (
                e.sys.thunks.get(e.thunk).module.clone(),
                if name == "__set_app_type" {
                    "app_type"
                } else if name == "__iob_func" || name == "__p__iob" {
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
                let size = if name == "__iob_func" || name == "__p__iob" {
                    128
                } else {
                    4
                };
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
            if !matches!(e.sys.objects.get(handle), Some(adv_machine::Object::Mutex { owner, .. }) if *owner == Some(e.context))
            {
                return Outcome::Fail("CRT unlock by a non-owner".into());
            }
            e.sys.release_mutex(handle);
            0
        }
        "malloc" | "_malloc_crt" | "??_U@YAPAXI@Z" => {
            if a > CRT_ALLOCATION_LIMIT {
                return Outcome::Return(0);
            }
            return crate::native_heap::allocate(e, a, false);
        }
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
        "calloc" => {
            let Some(size) = a
                .checked_mul(b)
                .filter(|&size| size <= CRT_ALLOCATION_LIMIT)
            else {
                return Outcome::Return(0);
            };
            return crate::native_heap::allocate(e, size, true);
        }
        "free" | "??_V@YAXPAX@Z" => {
            if a == 0 {
                return Outcome::Return(0);
            }
            return crate::native_heap::free(e, a);
        }
        "realloc" => {
            if a == 0 && b > CRT_ALLOCATION_LIMIT {
                return Outcome::Return(0);
            }
            return crate::native_heap::realloc(e, a, b);
        }
        "memcpy" | "memmove"
            if !e.cpu.accessible(b, c, adv_cpu::backend::Prot::READ)
                || !e.cpu.accessible(a, c, adv_cpu::backend::Prot::WRITE) =>
        {
            return memory_ops::guest_fault(e);
        }
        "memset" if !e.cpu.accessible(a, c, adv_cpu::backend::Prot::WRITE) => {
            return memory_ops::guest_fault(e);
        }
        "memcpy" | "memmove" => match memory_ops::copy(e, a, b, c) {
            Ok(pointer) => pointer,
            Err(err) => return Outcome::Fail(err.to_string()),
        },
        "memset" => match memory_ops::fill(e, a, b as u8, c) {
            Ok(pointer) => pointer,
            Err(err) => return Outcome::Fail(err.to_string()),
        },
        "memcmp" => match memory_ops::compare(e, a, b, c, None) {
            Ok(difference) => difference as u32,
            Err(error) => return Outcome::Fail(error.to_string()),
        },
        "strlen" | "strcmp" | "strncmp" | "strchr" | "strrchr" | "strstr" | "strcpy"
        | "strncpy" | "strcat" | "strncat" | "wcslen" | "wcscmp" | "wcsncmp" | "wcschr"
        | "wcsrchr" | "wcsstr" | "wcscpy" | "wcsncpy" | "wcscat" => return strings::call(e),
        "_initterm" | "_initterm_e" => {
            if b < a || b - a > 1024 * 1024 || !(b - a).is_multiple_of(4) {
                return Outcome::Fail("invalid initializer range".into());
            }
            return initializers(e, a, b, name == "_initterm_e");
        }
        "time" | "_time32" => {
            let ft = crate::kernel32::filetime_now(e);
            let unix_sec = (ft.saturating_sub(116_444_736_000_000_000) / 10_000_000) as u32;
            if a != 0
                && let Err(error) = e.cpu.write_u32(a, unix_sec)
            {
                return Outcome::Fail(error.to_string());
            }
            unix_sec
        }
        "_wcsicmp" => {
            let mut p1 = a;
            let mut p2 = b;
            loop {
                let (Ok(u1), Ok(u2)) = (e.cpu.read_u16(p1), e.cpu.read_u16(p2)) else {
                    return Outcome::Fail("CRT _wcsicmp unmapped".into());
                };
                let c1 = if u1 <= 127 {
                    (u1 as u8).to_ascii_lowercase() as u16
                } else {
                    u1
                };
                let c2 = if u2 <= 127 {
                    (u2 as u8).to_ascii_lowercase() as u16
                } else {
                    u2
                };
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
        // The "C" locale folds only ASCII letters and returns the byte difference.
        "_stricmp" | "_strcmpi" | "_strnicmp" => {
            let limit = if name == "_strnicmp" { c } else { u32::MAX };
            let mut offset = 0;
            while offset < limit {
                let (Ok(l), Ok(r)) = (
                    e.cpu.read_u8(a.wrapping_add(offset)),
                    e.cpu.read_u8(b.wrapping_add(offset)),
                ) else {
                    return Outcome::Fail(format!("CRT {name} unmapped"));
                };
                let (l, r) = (l.to_ascii_lowercase(), r.to_ascii_lowercase());
                if l != r || l == 0 {
                    return Outcome::Return((i32::from(l) - i32::from(r)) as u32);
                }
                offset += 1;
            }
            0
        }
        // abort exits with 3 and _amsg_exit with 255, as msvcrt does.
        "abort" => return crate::kernel32::exit_process(e, 3),
        "_amsg_exit" => return crate::kernel32::exit_process(e, 255),
        "_invalid_parameter_noinfo" => return invalid_parameter_noinfo(e),
        "gmtime" | "_gmtime32" => return time64::convert(e, a, false, false),
        "localtime" | "_localtime32" => return time64::convert(e, a, false, true),
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
                let c1 = if u1 <= 127 {
                    (u1 as u8).to_ascii_lowercase() as u16
                } else {
                    u1
                };
                let c2 = if u2 <= 127 {
                    (u2 as u8).to_ascii_lowercase() as u16
                } else {
                    u2
                };
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
        "fopen" | "_wfopen" | "fopen_s" | "_wfopen_s" | "fread" | "fwrite" | "fseek" | "ftell"
        | "fclose" | "fflush" | "feof" => return file::front(e, &name),
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
            let _ = e
                .cpu
                .write(dest.wrapping_add(dest_str.len() as u32), &src_str);
            let _ = e.cpu.write(
                dest.wrapping_add((dest_str.len() + src_str.len()) as u32),
                &[0],
            );
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
                let _ = e.cpu.write(
                    dest.wrapping_add(dest_str.len() as u32),
                    &src_str[..append_len],
                );
            }
            let _ = e.cpu.write(
                dest.wrapping_add((dest_str.len() + append_len) as u32),
                &[0],
            );
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
                if u == 0 {
                    break;
                }
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
            for (i, unit) in src_units.iter().take(copy_len).enumerate() {
                let _ = e
                    .cpu
                    .write(dest.wrapping_add((i * 2) as u32), &unit.to_le_bytes());
            }
            let _ = e
                .cpu
                .write(dest.wrapping_add((copy_len * 2) as u32), &[0, 0]);
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
                if u == 0 {
                    break;
                }
                src_units.push(u);
                p += 2;
            }
            if src_units.len() >= dest_size {
                let _ = e.cpu.write(dest, &[0, 0]);
                return Outcome::Return(34);
            }
            for (i, unit) in src_units.iter().enumerate() {
                let _ = e
                    .cpu
                    .write(dest.wrapping_add((i * 2) as u32), &unit.to_le_bytes());
            }
            let _ = e
                .cpu
                .write(dest.wrapping_add((src_units.len() * 2) as u32), &[0, 0]);
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
                if u == 0 {
                    break;
                }
                dest_len += 1;
                p += 2;
            }
            let mut src_units = Vec::new();
            let mut p = src;
            while let Ok(u) = e.cpu.read_u16(p) {
                if u == 0 {
                    break;
                }
                src_units.push(u);
                p += 2;
            }
            if dest_len + src_units.len() >= dest_size {
                let _ = e.cpu.write(dest, &[0, 0]);
                return Outcome::Return(34);
            }
            for (i, unit) in src_units.iter().enumerate() {
                let _ = e.cpu.write(
                    dest.wrapping_add(((dest_len + i) * 2) as u32),
                    &unit.to_le_bytes(),
                );
            }
            let _ = e.cpu.write(
                dest.wrapping_add(((dest_len + src_units.len()) * 2) as u32),
                &[0, 0],
            );
            0
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
        "atoi" => {
            let Ok(s) = e.cpu.read_cstr(a, 1024) else {
                return Outcome::Fail("CRT string unmapped".into());
            };
            let wide: Vec<u16> = s.iter().map(|&byte| byte as u16).collect();
            decimal_int(&wide) as u32
        }
        "abs" => (a as i32).unsigned_abs(),
        "_itoa" => {
            if !(2..=36).contains(&c) {
                return Outcome::Return(b);
            }
            // Only radix 10 is signed, as in msvcrt.
            let (negative, mut magnitude) = if c == 10 && (a as i32) < 0 {
                (true, (a as i32).unsigned_abs())
            } else {
                (false, a)
            };
            let mut text = Vec::new();
            loop {
                text.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(magnitude % c) as usize]);
                magnitude /= c;
                if magnitude == 0 {
                    break;
                }
            }
            if negative {
                text.push(b'-');
            }
            text.reverse();
            text.push(0);
            if let Err(error) = e.cpu.write(b, &text) {
                return Outcome::Fail(error.to_string());
            }
            b
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
            if is_lead { 1 } else { 0 }
        }
        "_ismbbtrail" => {
            let acp = e.ext.get::<crate::Win32>().codec.acp;
            let byte = a as u8;
            let is_trail = a <= 0xff
                && match acp {
                    936 => (0x40..=0x7e).contains(&byte) || (0x80..=0xfe).contains(&byte),
                    _ => (0x40..=0x7e).contains(&byte) || (0x80..=0xfc).contains(&byte),
                };
            if is_trail { 1 } else { 0 }
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
        "_setjmp" | "_setjmp3" => {
            let count = if name == "_setjmp3" { b } else { 0 };
            use adv_abi::{
                GuestPtr,
                guest::{check_range, memory_failure},
            };
            if let Err(fault) = check_range(e.cpu, a, 64, adv_cpu::backend::Prot::WRITE) {
                return memory_failure(fault);
            }
            let frame = match e.cpu.read_u32(e.ctx().teb) {
                Ok(frame) => frame,
                Err(fault) => return memory_failure(fault),
            };
            let level = if frame == u32::MAX {
                u32::MAX
            } else if count > 1 {
                e.arg(3)
            } else {
                match e.cpu.read_u32(frame.wrapping_add(12)) {
                    Ok(level) => level,
                    Err(fault) => return memory_failure(fault),
                }
            };
            // MSVC setjmp.h's x86 _JUMP_BUFFER is sixteen DWORDs. Compiler
            // arguments select the local unwind callback and its metadata.
            let prefix = [
                e.cpu.reg(Reg::Ebp),
                e.cpu.reg(Reg::Ebx),
                e.cpu.reg(Reg::Edi),
                e.cpu.reg(Reg::Esi),
                e.cpu.reg(Reg::Esp).wrapping_add(4),
                e.caller(),
                frame,
                level,
                JMP_MAGIC,
                if count > 0 && frame != u32::MAX {
                    e.arg(2)
                } else {
                    0
                },
            ];
            for (index, word) in prefix.into_iter().enumerate() {
                if let Err(fault) =
                    GuestPtr::new(a.wrapping_add(index as u32 * 4)).write(e.cpu, word)
                {
                    return memory_failure(fault);
                }
            }
            if frame != u32::MAX {
                for index in 0..count.saturating_sub(2).min(6) {
                    let word = e.arg(4 + index);
                    if let Err(fault) =
                        GuestPtr::new(a.wrapping_add(40 + index * 4)).write(e.cpu, word)
                    {
                        return memory_failure(fault);
                    }
                }
            }
            0
        }
        "longjmp" => return long_jump(e, a, b),
        // R6025: no `_set_purecall_handler` exists in this CRT, so it ends the
        // process the way `_amsg_exit` does.
        "_purecall" => return cpp_runtime::purecall(e),
        "getenv" => 0,
        "_makepath" => {
            let mut out = Vec::new();
            if b != 0
                && let Ok(drive) = e.cpu.read_cstr(b, 260)
            {
                out.extend_from_slice(&drive);
                if drive.len() == 1 && drive[0] != b':' {
                    out.push(b':');
                }
            }
            if c != 0
                && let Ok(dir) = e.cpu.read_cstr(c, 260)
            {
                out.extend_from_slice(&dir);
                if !dir.is_empty() && dir.last() != Some(&b'/') && dir.last() != Some(&b'\\') {
                    out.push(b'\\');
                }
            }
            let d = e.arg(3);
            if d != 0
                && let Ok(fname) = e.cpu.read_cstr(d, 260)
            {
                out.extend_from_slice(&fname);
            }
            let ext_ptr = e.arg(4);
            if ext_ptr != 0
                && let Ok(ext) = e.cpu.read_cstr(ext_ptr, 260)
            {
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
        "frexp" => {
            let x = match double(e) {
                Ok(v) => v,
                Err(err) => return Outcome::Fail(err),
            };
            let (result, exp) = split_fraction(x);
            if let Err(err) = e.cpu.write_u32(c, exp as u32) {
                return Outcome::Fail(err.to_string());
            }
            e.cpu.push_f64(result);
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

/// `strtol(text, NULL, 10)`, saturating like the msvcrt `atoi`.
fn decimal_int(text: &[u16]) -> i32 {
    let (value, _, error) = convert::parse(text, 10);
    let negative = text
        .iter()
        .find(|c| !matches!(c, 9..=13 | 32))
        .is_some_and(|c| *c == u16::from(b'-'));
    let magnitude = if negative {
        value.wrapping_neg()
    } else {
        value
    };
    match (negative, error) {
        (false, Some(_)) => i32::MAX,
        (true, Some(_)) => i32::MIN,
        (false, None) => magnitude.min(i32::MAX as u64) as i32,
        (true, None) => (-(magnitude.min(1 << 31) as i64)) as i32,
    }
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
    // spec: WIN32-API-18
    #[test]
    fn mingw_crt_imports_compare_ascii_keep_streams_and_exit_with_msvcrt_codes() {
        let mut m = machine(false);
        let (code, data) = (0x400000, 0x500000);
        m.cpu.map(code, 4096, Prot::RX).unwrap();
        m.cpu.map(data, 4096, Prot::RW).unwrap();
        let strings = ["ABC", "abd", "Hello", "hELLO", "A[", "a{", "abcX", "ABCy"];
        let mut addresses = Vec::new();
        for (index, text) in strings.iter().enumerate() {
            let address = data + 0x100 + index as u32 * 16;
            m.cpu
                .write(address, format!("{text}\0").as_bytes())
                .unwrap();
            addresses.push(address);
        }
        let api = |m: &mut Machine<JitCpu>, name| m.resolve_import("msvcrt.dll", name).unwrap();
        let (stricmp, strcmpi) = (api(&mut m, "_stricmp"), api(&mut m, "_strcmpi"));
        let (strnicmp, p_iob) = (api(&mut m, "_strnicmp"), api(&mut m, "__p__iob"));
        let abort = api(&mut m, "abort");
        let calls: [(u32, &[u32]); 6] = [
            (stricmp, &[addresses[0], addresses[1]]),
            (strcmpi, &[addresses[2], addresses[3]]),
            (stricmp, &[addresses[4], addresses[5]]),
            (strnicmp, &[addresses[6], addresses[7], 3]),
            (p_iob, &[]),
            (p_iob, &[]),
        ];
        let mut a = CodeAssembler::new(32).unwrap();
        for (index, (function, arguments)) in calls.iter().enumerate() {
            for &argument in arguments.iter().rev() {
                a.push(argument as i32).unwrap();
            }
            a.call(u64::from(*function)).unwrap();
            a.add(esp, 4 * arguments.len() as i32).unwrap();
            a.mov(dword_ptr((data + 4 * index as u32) as i32), eax)
                .unwrap();
        }
        a.call(u64::from(abort)).unwrap();
        m.cpu
            .write(code, &a.assemble(u64::from(code)).unwrap())
            .unwrap();
        m.spawn("mingw-crt", code, &[], 65536).unwrap();
        let result = m.run();
        assert!(matches!(result, RunResult::Exited(3)), "{result:?}");
        let value =
            |m: &Machine<JitCpu>, index: u32| m.cpu.read_u32(data + 4 * index).unwrap() as i32;
        assert_eq!(value(&m, 0), -1, "c - d after folding");
        assert_eq!(value(&m, 1), 0);
        assert_eq!(value(&m, 2), 0x5b - 0x7b, "only letters fold");
        assert_eq!(value(&m, 3), 0, "the limit stops before X/y");
        assert_ne!(value(&m, 4), 0);
        assert_eq!(value(&m, 4), value(&m, 5));
    }
    #[test]
    fn longjmp_resumes_setjmp3_with_saved_registers_and_a_nonzero_value() {
        for jit in [false, true] {
            let mut m = machine(jit);
            let (code, data) = (0x400000, 0x500000);
            let buffer = data + 0x100;
            m.cpu.map(code, 4096, Prot::RX).unwrap();
            m.cpu.map(data, 4096, Prot::RW).unwrap();
            let api = |m: &mut Machine<JitCpu>, name| m.resolve_import("msvcrt.dll", name).unwrap();
            let (setjmp, longjmp) = (api(&mut m, "_setjmp3"), api(&mut m, "longjmp"));
            let abort = api(&mut m, "abort");
            let slot = |offset: u32| dword_ptr((data + offset) as i32);
            let mut a = CodeAssembler::new(32).unwrap();
            let mut done = a.create_label();
            a.mov(ebx, 0x1234_5678).unwrap();
            a.mov(esi, 0x1111).unwrap();
            a.mov(edi, 0x2222).unwrap();
            a.mov(ebp, 0x3333).unwrap();
            a.mov(slot(8), esp).unwrap();
            a.push(0).unwrap();
            a.push(buffer as i32).unwrap();
            a.call(u64::from(setjmp)).unwrap();
            a.add(esp, 8).unwrap();
            a.inc(slot(0)).unwrap();
            a.mov(slot(4), eax).unwrap();
            a.test(eax, eax).unwrap();
            a.jne(done).unwrap();
            for register in [ebx, esi, edi, ebp] {
                a.xor(register, register).unwrap();
            }
            a.push(0).unwrap();
            a.push(buffer as i32).unwrap();
            a.call(u64::from(longjmp)).unwrap();
            a.call(u64::from(abort)).unwrap();
            a.set_label(&mut done).unwrap();
            for (offset, register) in [(12, ebx), (16, esi), (20, edi), (24, ebp), (28, esp)] {
                a.mov(slot(offset), register).unwrap();
            }
            a.call(u64::from(abort)).unwrap();
            m.cpu
                .write(code, &a.assemble(u64::from(code)).unwrap())
                .unwrap();
            m.spawn("longjmp", code, &[], 65536).unwrap();
            let result = m.run();
            assert!(matches!(result, RunResult::Exited(3)), "{result:?}");
            let value = |offset| m.cpu.read_u32(data + offset).unwrap();
            assert_eq!(value(0), 2, "setjmp returns twice");
            assert_eq!(value(4), 1, "longjmp(buf, 0) returns 1");
            assert_eq!(
                [value(12), value(16), value(20), value(24), value(28)],
                [0x1234_5678, 0x1111, 0x2222, 0x3333, value(8)]
            );
            assert_eq!(m.cpu.read_u32(buffer + 32).unwrap(), JMP_MAGIC);
        }
    }
    #[test]
    fn purecall_ends_the_process_like_amsg_exit() {
        let mut m = machine(false);
        let code = 0x400000;
        m.cpu.map(code, 4096, Prot::RX).unwrap();
        let purecall = m.resolve_import("msvcrt.dll", "_purecall").unwrap();
        let mut a = CodeAssembler::new(32).unwrap();
        a.call(u64::from(purecall)).unwrap();
        m.cpu
            .write(code, &a.assemble(u64::from(code)).unwrap())
            .unwrap();
        m.spawn("purecall", code, &[], 65536).unwrap();
        let result = m.run();
        assert!(matches!(result, RunResult::Exited(255)), "{result:?}");
    }
    #[test]
    fn vcruntime_memset_fills_only_the_requested_bytes_returns_destination_and_is_cdecl() {
        for jit in [false, true] {
            let mut m = machine(jit);
            let (code, data) = (0x400000, 0x500000);
            let length = 4099;
            let buffer = data + 64;
            let destination = buffer + 7;
            m.cpu.map(code, 4096, Prot::RX).unwrap();
            m.cpu.map(data, 8192, Prot::RW).unwrap();
            m.cpu.write(buffer, &vec![0xa5; length + 14]).unwrap();
            let memset = m.resolve_import("vcruntime140.dll", "memset").unwrap();
            let mut a = CodeAssembler::new(32).unwrap();
            a.mov(dword_ptr((data + 4) as i32), esp).unwrap();
            a.push(length as i32).unwrap();
            a.push(0x1234).unwrap();
            a.push(destination).unwrap();
            a.call(u64::from(memset)).unwrap();
            a.mov(dword_ptr(data as i32), eax).unwrap();
            a.mov(dword_ptr((data + 8) as i32), esp).unwrap();
            a.add(esp, 12).unwrap();
            a.mov(dword_ptr((data + 12) as i32), esp).unwrap();
            a.xor(eax, eax).unwrap();
            a.ret().unwrap();
            m.cpu
                .write(code, &a.assemble(u64::from(code)).unwrap())
                .unwrap();
            m.spawn("vcruntime-memset", code, &[], 65536).unwrap();
            let result = m.run();
            assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
            assert_eq!(m.cpu.read_u32(data).unwrap(), destination);
            let before = m.cpu.read_u32(data + 4).unwrap();
            assert_eq!(
                m.cpu.read_u32(data + 8).unwrap(),
                before - 12,
                "caller retains all three arguments"
            );
            assert_eq!(m.cpu.read_u32(data + 12).unwrap(), before);
            let mut bytes = vec![0; length + 14];
            m.cpu.read(buffer, &mut bytes).unwrap();
            assert_eq!(&bytes[..7], &[0xa5; 7]);
            assert_eq!(&bytes[7..7 + length], &vec![0x34; length]);
            assert_eq!(&bytes[7 + length..], &[0xa5; 7]);
        }
    }
    #[test]
    fn vcruntime_memcpy_and_overlapping_memmove_preserve_bytes_and_cdecl_contract() {
        for jit in [false, true] {
            for (name, source_offset, destination_offset) in
                [("memcpy", 0, 64), ("memmove", 0, 5), ("memmove", 5, 0)]
            {
                let mut m = machine(jit);
                let (code, data) = (0x400000, 0x500000);
                let buffer = data + 64;
                let length = 21;
                let source = buffer + source_offset as u32;
                let destination = buffer + destination_offset as u32;
                let bytes: Vec<_> = (0..128).map(|i| i as u8).collect();
                let mut expected = bytes.clone();
                expected.copy_within(source_offset..source_offset + length, destination_offset);
                m.cpu.map(code, 4096, Prot::RX).unwrap();
                m.cpu.map(data, 4096, Prot::RW).unwrap();
                m.cpu.write(buffer, &bytes).unwrap();
                let function = m.resolve_import("vcruntime140.dll", name).unwrap();
                let mut a = CodeAssembler::new(32).unwrap();
                a.mov(dword_ptr((data + 4) as i32), esp).unwrap();
                a.push(length as i32).unwrap();
                a.push(source).unwrap();
                a.push(destination).unwrap();
                a.call(u64::from(function)).unwrap();
                a.mov(dword_ptr(data as i32), eax).unwrap();
                a.mov(dword_ptr((data + 8) as i32), esp).unwrap();
                a.add(esp, 12).unwrap();
                a.mov(dword_ptr((data + 12) as i32), esp).unwrap();
                a.xor(eax, eax).unwrap();
                a.ret().unwrap();
                m.cpu
                    .write(code, &a.assemble(u64::from(code)).unwrap())
                    .unwrap();
                m.spawn("vcruntime-copy", code, &[], 65536).unwrap();
                let result = m.run();
                assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
                assert_eq!(m.cpu.read_u32(data).unwrap(), destination, "{name}");
                let before = m.cpu.read_u32(data + 4).unwrap();
                assert_eq!(m.cpu.read_u32(data + 8).unwrap(), before - 12, "{name}");
                assert_eq!(m.cpu.read_u32(data + 12).unwrap(), before, "{name}");
                let mut actual = vec![0; bytes.len()];
                m.cpu.read(buffer, &mut actual).unwrap();
                assert_eq!(
                    actual, expected,
                    "{name}: {source_offset}->{destination_offset}"
                );
            }
        }
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
            assert!(m.ext.get::<Heaps>().owner(&m.cpu, original).is_none());
            assert!(m.ext.get::<Heaps>().owner(&m.cpu, resized).is_none());
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
                    assert_eq!(
                        env.ext.get_mut::<Heaps>().size(env.cpu, heap, address),
                        None
                    );
                }
                assert_eq!(
                    env.ext.get_mut::<Heaps>().size(env.cpu, heap, cells[2]),
                    Some(4)
                );
                // Repeated notification cleanup is idempotent.
                detach_thread(env).unwrap();
                detach_process(env).unwrap();
                detach_process(env).unwrap();
                assert!(env.ext.get::<CrtState>().errno.is_empty());
                assert_eq!(
                    env.ext.get_mut::<Heaps>().size(env.cpu, heap, cells[2]),
                    None
                );
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
