//! Modules and resources: LoadLibrary, GetProcAddress, GetModuleHandle/FileName,
//! FindResource & co.

use std::collections::VecDeque;

use adv_abi::{
    AnsiStr, ApiDiagnostics, DynamicLookupFailure, DynamicLookupKind, Handle, Registry, WideStr,
    win32,
};
use adv_cpu::backend::{GuestExt, MemFault, Prot};
use adv_machine::outcome::DeferredContinuation;
use adv_machine::{Env, InitCall, Object, Outcome, Wait};
use adv_os::{Loader, ModuleId, ModuleKind, Modules};
use adv_pe::{ImportName, ResId};

use super::K32;
use crate::util::{IntOrStr, cstr, err, put_cstr, put_wstr, set_last_error, wstr};
use crate::{Win32, stdcall};

mod datafile;
mod notifications;
pub(crate) use notifications::prepare_system_modules;
pub(crate) use notifications::{exit_process, exit_thread, install_startup, spawn_thread};

fn release_lock(env: &mut Env<'_>, handle: u32, outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Call {
            target,
            args,
            then,
            stack,
            unwindable,
        } => Outcome::Call {
            target,
            args,
            stack,
            unwindable,
            then: Box::new(move |env, resume| {
                let next = then(env, resume);
                release_lock(env, handle, next)
            }),
        },
        Outcome::Block {
            wait,
            then,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            allow_time_skip,
            then: Box::new(move |env, resume| {
                let next = then(env, resume);
                release_lock(env, handle, next)
            }),
        },
        Outcome::Defer { then } => Outcome::defer(move |env| {
            let next = then(env);
            release_lock(env, handle, next)
        }),
        terminal => {
            if let Some(Object::Mutex { owner, recursion }) = env.sys.objects.get_mut(handle) {
                debug_assert_eq!(*owner, Some(env.context));
                *recursion -= 1;
                if *recursion == 0 {
                    *owner = None;
                }
            }
            terminal
        }
    }
}

fn loader_mutex(env: &mut Env<'_>) -> Option<u32> {
    let handle = native_loader_mutex(env);
    if handle.is_none() {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
    }
    handle
}

pub(crate) fn native_loader_mutex(env: &mut Env<'_>) -> Option<u32> {
    match env.ext.get::<Win32>().loader_lock {
        Some(handle) => Some(handle),
        None => {
            let handle = env.sys.objects.insert(Object::Mutex {
                owner: None,
                recursion: 0,
            });
            if handle == 0 {
                return None;
            }
            env.ext.get_mut::<Win32>().loader_lock = Some(handle);
            Some(handle)
        }
    }
}

fn locked(env: &mut Env<'_>, operation: impl FnOnce(&mut Env<'_>) -> Outcome + 'static) -> Outcome {
    locked_profile(env, false, operation)
}

fn locked_profile(
    env: &mut Env<'_>,
    native: bool,
    operation: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let handle = if native {
        native_loader_mutex(env)
    } else {
        loader_mutex(env)
    };
    let Some(handle) = handle else {
        return Outcome::Return(if native { 0xc0000017 } else { 0 });
    };
    if env.sys.try_acquire_objects(&[handle], false).is_some() {
        let result = operation(env);
        release_lock(env, handle, result)
    } else {
        Outcome::block(
            Wait::Objects {
                handles: vec![handle],
                all: false,
                deadline: None,
                notification: None,
            },
            move |env, _| {
                let result = operation(env);
                release_lock(env, handle, result)
            },
        )
    }
}

fn retire(env: &mut Env<'_>, id: ModuleId) -> Result<(), String> {
    let (win32, modules) = env.ext.get2_mut::<Win32, Modules>();
    Loader::new(env.cpu, env.sys, modules, &win32.vfs)
        .retire(id)
        .map_err(|error| error.to_string())
}

fn run_detaches(
    env: &mut Env<'_>,
    mut modules: VecDeque<ModuleId>,
    mut calls: VecDeque<InitCall>,
    result: u32,
    error: Option<u32>,
) -> Outcome {
    loop {
        if let Some(call) = calls.pop_front() {
            return Outcome::call(call.target, call.args, move |env, _| {
                run_detaches(env, modules, calls, result, error)
            });
        }
        if let Some(id) = modules.pop_front()
            && let Err(error) = retire(env, id)
        {
            return Outcome::Fail(format!("module retirement: {error}"));
        }
        let Some(&id) = modules.front() else {
            break;
        };
        calls = detach_calls(env, id);
    }
    if let Some(error) = error {
        set_last_error(env, error);
    }
    Outcome::Return(result)
}

fn detach_calls(env: &Env<'_>, id: ModuleId) -> VecDeque<InitCall> {
    let module = adv_os::modules(env.ext).get(id);
    if !module.lifetime.attach_started {
        return VecDeque::new();
    }
    let mut targets = module.lifetime.callbacks.clone();
    if module.is_dll && module.entry != 0 {
        targets.push(module.entry);
    }
    targets
        .into_iter()
        .map(|target| InitCall {
            target,
            args: vec![module.base, 0, 0],
            what: format!("detach {}", module.name),
            failure_exit: None,
            begin: None,
        })
        .collect()
}

fn unload(env: &mut Env<'_>, ids: Vec<ModuleId>, result: u32, error: Option<u32>) -> Outcome {
    for &id in &ids {
        adv_os::modules_mut(env.ext).get_mut(id).lifetime.unloading = true;
    }
    let ids: VecDeque<_> = ids.into();
    let calls = ids
        .front()
        .map(|&id| detach_calls(env, id))
        .unwrap_or_default();
    run_detaches(env, ids, calls, result, error)
}

#[win32(kernel32, name = "FreeLibrary", level = implement)]
fn free_library(env: &mut Env<'_>, handle: Handle) -> Outcome {
    locked(env, move |env| {
        let Some(id) = adv_os::modules(env.ext).by_handle(handle.0) else {
            set_last_error(env, err::MOD_NOT_FOUND);
            return Outcome::Return(0);
        };
        match adv_os::modules_mut(env.ext).release(env.cpu, id) {
            Ok(ids) => unload(env, ids, 1, None),
            Err(error) => Outcome::Fail(error.to_string()),
        }
    })
}

#[win32(kernel32, name = "FreeLibraryAndExitThread", level = implement)]
fn free_library_and_exit_thread(env: &mut Env<'_>, handle: Handle, code: u32) -> Outcome {
    let released = free_library(env, handle);
    // Finish every detach and release the loader lock before ending the thread.
    // No guest return may reach the caller, whose DLL may now be unmapped.
    notifications::after(env, released, Box::new(move |env| exit_thread(env, code)))
}

/// Run dependency initializers before the requested image. Only guest returns
/// enter this continuation; a failed DllMain unwinds the acquired dependency graph.
fn run_inits(
    env: &mut Env<'_>,
    mut inits: VecDeque<InitCall>,
    finished: DeferredContinuation,
    failed: DeferredContinuation,
) -> Outcome {
    match inits.pop_front() {
        None => finished(env),
        Some(c) => {
            log::debug!("LoadLibrary init: {}", c.what);
            let is_dllmain = c.failure_exit.is_some();
            if let Some((begin, token)) = c.begin {
                begin(env, token);
            }
            Outcome::call(c.target, c.args, move |env, resume| {
                if is_dllmain && resume.value() == 0 {
                    failed(env)
                } else {
                    run_inits(env, inits, finished, failed)
                }
            })
        }
    }
}

fn load_library(env: &mut Env<'_>, name: String) -> Outcome {
    load_library_search(env, name, false)
}

fn load_library_search(env: &mut Env<'_>, name: String, altered: bool) -> Outcome {
    load_library_front(env, name, altered, LibraryFront::Win32)
}

#[derive(Clone, Copy)]
enum LibraryFront {
    Win32,
    Native(u32),
}

pub(crate) fn native_load_library(env: &mut Env<'_>, name: String, output: u32) -> Outcome {
    locked_profile(env, true, move |env| {
        // The caller may have lost the output while waiting for the loader lock.
        if adv_abi::guest::check_range(env.cpu, output, 4, Prot::WRITE).is_err() {
            return Outcome::Return(0xc0000005);
        }
        load_library_front(env, name, false, LibraryFront::Native(output))
    })
}

fn release_loaded(env: &mut Env<'_>, id: ModuleId, result: u32, error: Option<u32>) -> Outcome {
    let ids = match adv_os::modules_mut(env.ext).release(env.cpu, id) {
        Ok(ids) => ids,
        Err(error) => return Outcome::Fail(error.to_string()),
    };
    unload(env, ids, result, error)
}

fn load_library_front(
    env: &mut Env<'_>,
    name: String,
    altered: bool,
    front: LibraryFront,
) -> Outcome {
    let (w, modules) = env.ext.get2_mut::<Win32, Modules>();
    let mut l = Loader::new(&mut *env.cpu, &mut *env.sys, modules, &w.vfs);
    let res = if altered {
        l.load_altered(&name)
    } else {
        l.load(&name)
    };
    let inits: VecDeque<InitCall> = std::mem::take(&mut l.inits).into();
    drop(l);
    match res {
        Ok(id) => {
            let base = adv_os::modules(env.ext).get(id).base;
            log::info!("LoadLibrary({name}) = {base:#010x}");
            for call in &inits {
                if let Some(id) = adv_os::modules(env.ext).by_handle(call.args[0]) {
                    adv_os::modules_mut(env.ext)
                        .get_mut(id)
                        .lifetime
                        .attach_started = false;
                }
            }
            run_inits(
                env,
                inits,
                Box::new(move |env| match front {
                    LibraryFront::Win32 => Outcome::Return(base),
                    LibraryFront::Native(output) => {
                        if adv_abi::guest::check_range(env.cpu, output, 4, Prot::WRITE).is_err()
                            || env.cpu.write_u32(output, base).is_err()
                        {
                            // Guest initializers can unmap the caller's output.
                            // Release only the reference acquired by this call.
                            release_loaded(env, id, 0xc0000005, None)
                        } else {
                            Outcome::Return(0)
                        }
                    }
                }),
                Box::new(move |env| match front {
                    LibraryFront::Win32 => release_loaded(env, id, 0, Some(1114)),
                    LibraryFront::Native(_) => release_loaded(env, id, 0xc0000142, None),
                }),
            )
        }
        Err(e) => {
            log::warn!("LoadLibrary({name}) failed: {e}");
            match front {
                LibraryFront::Win32 => {
                    set_last_error(env, load_error_code(&e));
                    Outcome::Return(0)
                }
                LibraryFront::Native(_) => Outcome::Return(match e {
                    adv_os::LoadError::Pe { .. } | adv_os::LoadError::NoRelocs(..) => 0xc000007b,
                    adv_os::LoadError::NoSpace(..)
                    | adv_os::LoadError::TooManyHostModules
                    | adv_os::LoadError::Mem(_) => 0xc0000017,
                    adv_os::LoadError::MissingExport { .. } => 0xc0000139,
                    adv_os::LoadError::NotFound(_) => 0xc0000135,
                }),
            }
        }
    }
}

fn load_error_code(error: &adv_os::LoadError) -> u32 {
    match error {
        adv_os::LoadError::Pe { .. } | adv_os::LoadError::NoRelocs(..) => 193,
        adv_os::LoadError::NoSpace(..)
        | adv_os::LoadError::TooManyHostModules
        | adv_os::LoadError::Mem(_) => err::NOT_ENOUGH_MEMORY,
        adv_os::LoadError::MissingExport { .. } => err::PROC_NOT_FOUND,
        adv_os::LoadError::NotFound(_) => err::MOD_NOT_FOUND,
    }
}

fn load_library_ex(env: &mut Env<'_>, name: String, file: Handle, flags: u32) -> Outcome {
    if file.0 != 0
        || flags & !0x3ffb != 0
        || flags & 0x42 == 0x42
        || (flags & 8 != 0 && flags & 0x1f00 != 0)
    {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    }
    match flags {
        0 => locked(env, move |env| load_library(env, name)),
        8 => locked(env, move |env| load_library_search(env, name, true)),
        2 => locked(env, move |env| datafile::load(env, name)),
        _ => {
            // IMAGE_RESOURCE/EXCLUSIVE need generic file leases, and DONT_RESOLVE
            // needs executable-but-uninitialized lifetime. Never run DllMain.
            set_last_error(env, 50); // ERROR_NOT_SUPPORTED
            Outcome::Return(0)
        }
    }
}

#[win32(kernel32, name = "LoadLibraryExA", level = implement)]
fn load_library_ex_a(env: &mut Env<'_>, name: AnsiStr, file: Handle, flags: u32) -> Outcome {
    let Ok(bytes) = name.read(env.cpu, 32768) else {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    };
    let name = env.ext.get::<Win32>().codec.decode(&bytes);
    load_library_ex(env, name, file, flags)
}

#[win32(kernel32, name = "LoadLibraryExW", level = implement)]
fn load_library_ex_w(env: &mut Env<'_>, name: WideStr, file: Handle, flags: u32) -> Outcome {
    let Ok(units) = name.read(env.cpu, 32768) else {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    };
    load_library_ex(env, String::from_utf16_lossy(&units), file, flags)
}

fn arg_a(env: &Env<'_>, i: u32) -> String {
    env.ext
        .get::<Win32>()
        .codec
        .decode(&cstr(env.cpu, env.arg(i)))
}

fn arg_w(env: &Env<'_>, i: u32) -> String {
    String::from_utf16_lossy(&wstr(env.cpu, env.arg(i)))
}

/// Module for an HMODULE argument (NULL = the exe).
pub(crate) fn module_of(env: &Env<'_>, h: u32) -> Option<ModuleId> {
    if h == 0 {
        let exe = env
            .cpu
            .read_u32(adv_machine::layout::PEB_ADDR + adv_machine::layout::peb::IMAGE_BASE)
            .ok()?;
        return adv_os::modules(env.ext).by_handle(exe);
    }
    adv_os::modules(env.ext).by_handle(h)
}

fn get_module_handle(env: &mut Env<'_>, name: Option<String>) -> Outcome {
    let id = match &name {
        None => module_of(env, 0),
        Some(n) => adv_os::modules(env.ext).by_name(n),
    };
    match id {
        Some(id) => Outcome::Return(adv_os::modules(env.ext).get(id).base),
        None => {
            log::debug!("GetModuleHandle({name:?}): not loaded");
            set_last_error(env, err::MOD_NOT_FOUND);
            Outcome::Return(0)
        }
    }
}

/// GetModuleHandleEx retains by default. PIN lasts until process exit; UNCHANGED
/// only observes and must not extend the image lifetime.
fn get_module_handle_ex(env: &mut Env<'_>, wide: bool) -> Outcome {
    const FROM_ADDRESS: u32 = 4;
    let (flags, name, out) = (env.arg(0), env.arg(1), env.arg(2));
    if out == 0 || env.cpu.write_u32(out, 0).is_err() || flags & !7 != 0 || flags & 3 == 3 {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    }
    let base = if flags & FROM_ADDRESS != 0 {
        let modules = adv_os::modules(env.ext);
        modules.containing(name).map(|id| modules.get(id).base)
    } else if name == 0 {
        module_of(env, 0).map(|id| adv_os::modules(env.ext).get(id).base)
    } else {
        let n = if wide { arg_w(env, 1) } else { arg_a(env, 1) };
        adv_os::modules(env.ext)
            .by_name(&n)
            .map(|id| adv_os::modules(env.ext).get(id).base)
    };
    match base {
        Some(base) => {
            let id = adv_os::modules(env.ext).by_handle(base).unwrap();
            if flags & 2 == 0
                && let Err(error) = adv_os::modules_mut(env.ext).retain(env.cpu, id)
            {
                return Outcome::Fail(error.to_string());
            }
            if flags & 1 != 0 {
                adv_os::modules_mut(env.ext).get_mut(id).lifetime.pinned = true;
            }
            let _ = env.cpu.write_u32(out, base);
            Outcome::Return(1)
        }
        None => {
            set_last_error(env, err::MOD_NOT_FOUND);
            Outcome::Return(0)
        }
    }
}

/// Fixed guest path of a module. Published DLL names belong to their LDR block;
/// resolving their original relative request against a later cwd is incorrect.
pub(super) fn module_path(env: &Env<'_>, id: ModuleId) -> Result<String, MemFault> {
    let w = env.ext.get::<Win32>();
    let m = adv_os::modules(env.ext).get(id);
    if m.ldr_entry == 0 {
        return match m.kind {
            // Stable configuration owners exist before publication. Never use
            // cwd to invent a DLL's missing name.
            ModuleKind::Host => Ok(format!("C:\\Windows\\system32\\{}", m.name)),
            ModuleKind::Image if !m.is_dll => Ok(w.exe_path()),
            _ => Err(MemFault {
                addr: m.base,
                len: m.size,
            }),
        };
    }
    let fault = MemFault {
        addr: m.ldr_entry,
        len: m.ldr_size,
    };
    let end = m.ldr_entry.checked_add(m.ldr_size).ok_or(fault)?;
    let header = m.ldr_entry.checked_add(0x24).ok_or(fault)?;
    let strings = m.ldr_entry.checked_add(0x80).ok_or(fault)?;
    if strings > end {
        return Err(fault);
    }
    adv_abi::guest::check_range(env.cpu, header, 8, Prot::READ)?;
    let mut descriptor = [0; 8];
    env.cpu.read(header, &mut descriptor)?;
    let length = u32::from(u16::from_le_bytes(descriptor[..2].try_into().unwrap()));
    let capacity = u32::from(u16::from_le_bytes(descriptor[2..4].try_into().unwrap()));
    let address = u32::from_le_bytes(descriptor[4..8].try_into().unwrap());
    if length == 0
        || length & 1 != 0
        || capacity & 1 != 0
        || length > capacity
        || address < strings
        || address
            .checked_add(capacity)
            .is_none_or(|end_of_string| end_of_string > end)
    {
        return Err(fault);
    }
    adv_abi::guest::check_range(env.cpu, address, length, Prot::READ)?;
    let mut bytes = vec![0; length as usize];
    env.cpu.read(address, &mut bytes)?;
    let words: Vec<_> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    if words.contains(&0) {
        return Err(fault);
    }
    String::from_utf16(&words).map_err(|_| fault)
}

fn get_module_file_name(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (h, buf, cap) = (env.arg(0), env.arg(1), env.arg(2));
    let Some(id) = module_of(env, h) else {
        set_last_error(env, err::MOD_NOT_FOUND);
        return Outcome::Return(0);
    };
    if !adv_os::modules(env.ext).get(id).executable_visible() {
        set_last_error(env, err::MOD_NOT_FOUND);
        return Outcome::Return(0);
    }
    let path = match module_path(env, id) {
        Ok(path) => path,
        Err(_) => {
            let api = if wide {
                "kernel32.dll!GetModuleFileNameW"
            } else {
                "kernel32.dll!GetModuleFileNameA"
            };
            adv_abi::warn_unavailable(
                env,
                api,
                "published module path is unreadable or outside its owned LDR FullDllName storage",
            );
            set_last_error(env, 998); // ERROR_NOACCESS; no mutable-cwd fallback
            return Outcome::Return(0);
        }
    };
    let r = if wide {
        let u: Vec<u16> = path.encode_utf16().collect();
        put_wstr(env.cpu, buf, cap, &u)
    } else {
        let b = env.ext.get::<Win32>().codec.encode(&path);
        put_cstr(env.cpu, buf, cap, &b)
    };
    match r {
        Some(n) => Outcome::Return(n),
        None => {
            set_last_error(env, err::INSUFFICIENT_BUFFER);
            Outcome::Return(cap)
        }
    }
}

fn get_proc_address(env: &mut Env<'_>) -> Outcome {
    let (h, name_p) = (env.arg(0), env.arg(1));
    let caller = env.caller();
    let sym = if name_p < 0x1_0000 {
        ImportName::Ordinal(name_p as u16)
    } else {
        ImportName::Name {
            hint: 0,
            name: String::from_utf8_lossy(&cstr(env.cpu, name_p)).into_owned(),
        }
    };
    locked(env, move |env| {
        resolve_proc(env, h, sym, caller, ProcedureFront::Win32)
    })
}

pub(crate) fn native_get_procedure(
    env: &mut Env<'_>,
    module: u32,
    name: ImportName,
    output: u32,
) -> Outcome {
    let caller = env.caller();
    locked_profile(env, true, move |env| {
        resolve_proc(env, module, name, caller, ProcedureFront::Native(output))
    })
}

#[derive(Clone, Copy)]
enum ProcedureFront {
    Win32,
    Native(u32),
}

fn procedure_result(env: &mut Env<'_>, front: ProcedureFront, address: u32) -> Outcome {
    match front {
        ProcedureFront::Win32 => Outcome::Return(address),
        ProcedureFront::Native(output) => {
            if adv_abi::guest::check_range(env.cpu, output, 4, Prot::WRITE).is_err()
                || env.cpu.write_u32(output, address).is_err()
            {
                Outcome::Return(0xc0000005)
            } else {
                Outcome::Return(0)
            }
        }
    }
}

fn procedure_error(env: &mut Env<'_>, front: ProcedureFront, error: u32, status: u32) -> Outcome {
    match front {
        ProcedureFront::Win32 => {
            set_last_error(env, error);
            Outcome::Return(0)
        }
        ProcedureFront::Native(_) => Outcome::Return(status),
    }
}

fn record_lookup_failure(
    env: &mut Env<'_>,
    module: String,
    sym: &ImportName,
    caller: u32,
    kind: DynamicLookupKind,
    reason: String,
    target: Option<(String, String)>,
) {
    let (failure_module, failure_name) = match target {
        Some((module, name)) => (Some(module), Some(name)),
        None => (None, None),
    };
    let failure = DynamicLookupFailure {
        sequence: 0,
        module,
        name: sym.to_string(),
        failure_module,
        failure_name,
        caller,
        context: env.context,
        kind,
        reason,
    };
    env.ext
        .get_or_default::<ApiDiagnostics>()
        .record_dynamic_lookup_failure(failure);
}

fn missing_lookup_kind(env: &Env<'_>, module: &str, name: &str) -> DynamicLookupKind {
    // A shipped guest DLL may share a system DLL's name. Only an unambiguous
    // live Host target proves this failure concerns our SDK capability, rather
    // than a guest export or an unknown optional name. Pinned Host modules
    // remain available after the loader rolls back a failed forwarder.
    let mut host = false;
    for (_, loaded) in adv_os::modules(env.ext).iter() {
        if loaded.executable_visible() && loaded.name.eq_ignore_ascii_case(module) {
            if loaded.kind != ModuleKind::Host {
                return DynamicLookupKind::MissingExport;
            }
            host = true;
        }
    }
    let registry = (env.sys.thunks.resolver() as &dyn std::any::Any).downcast_ref::<Registry>();
    if host && let Some(registry) = registry {
        let canonical = registry.canonical_name(module, name).unwrap_or(name);
        if adv_abi::signature::lookup(module, canonical).is_some()
            && !registry.dynamic_available(module, name)
        {
            return DynamicLookupKind::Unimplemented;
        }
    }
    DynamicLookupKind::MissingExport
}

fn resolve_proc(
    env: &mut Env<'_>,
    h: u32,
    sym: ImportName,
    caller: u32,
    front: ProcedureFront,
) -> Outcome {
    let requested = module_of(env, h);
    let Some(id) = requested.filter(|&id| adv_os::modules(env.ext).get(id).executable_visible())
    else {
        let module = requested
            .map(|id| adv_os::modules(env.ext).get(id).name.clone())
            .unwrap_or_else(|| "<invalid-hmodule>".into());
        record_lookup_failure(
            env,
            module,
            &sym,
            caller,
            DynamicLookupKind::InvalidModule,
            format!("HMODULE {h:#010x} is not an executable module"),
            None,
        );
        env.sys
            .trace
            .resolution(caller, "<invalid-hmodule>", &sym, 0);
        return procedure_error(env, front, err::MOD_NOT_FOUND, 0xc0000135);
    };
    let name = adv_os::modules(env.ext).get(id).name.clone();
    let (w, modules) = env.ext.get2_mut::<Win32, Modules>();
    let mut l = Loader::new(&mut *env.cpu, &mut *env.sys, modules, &w.vfs);
    let r = l.resolve_dynamic(id, &sym);
    let inits: VecDeque<_> = std::mem::take(&mut l.inits).into();
    drop(l);
    match r {
        Ok((address, changes)) => {
            if inits.is_empty() {
                env.sys.trace.resolution(caller, &name, &sym, address);
                return procedure_result(env, front, address);
            }
            let success_name = name.clone();
            let success_sym = sym.clone();
            run_inits(
                env,
                inits,
                Box::new(move |env| {
                    env.sys
                        .trace
                        .resolution(caller, &success_name, &success_sym, address);
                    procedure_result(env, front, address)
                }),
                Box::new(move |env| {
                    record_lookup_failure(
                        env,
                        name.clone(),
                        &sym,
                        caller,
                        DynamicLookupKind::InitializationFailed,
                        "a newly loaded forwarder dependency rejected process initialization"
                            .into(),
                        None,
                    );
                    env.sys.trace.resolution(caller, &name, &sym, 0);
                    let (w, modules) = env.ext.get2_mut::<Win32, Modules>();
                    match Loader::new(env.cpu, env.sys, modules, &w.vfs).abort_resolution(changes) {
                        Ok(ids) => match front {
                            ProcedureFront::Win32 => unload(env, ids, 0, Some(err::PROC_NOT_FOUND)),
                            ProcedureFront::Native(_) => unload(env, ids, 0xc0000142, None),
                        },
                        Err(error) => Outcome::Fail(error.to_string()),
                    }
                }),
            )
        }
        Err(e) => {
            let (kind, target) = match &e {
                adv_os::LoadError::MissingExport { module, name } => (
                    missing_lookup_kind(env, module, name),
                    Some((module.clone(), name.clone())),
                ),
                _ => (DynamicLookupKind::ResolutionFailed, None),
            };
            record_lookup_failure(env, name.clone(), &sym, caller, kind, e.to_string(), target);
            log::info!("GetProcAddress: {e}");
            env.sys.trace.resolution(caller, &name, &sym, 0);
            let status = match &e {
                adv_os::LoadError::MissingExport { .. } => match sym {
                    ImportName::Ordinal(_) => 0xc0000138,
                    ImportName::Name { .. } => 0xc0000139,
                },
                _ => 0xc0000135,
            };
            procedure_error(env, front, err::PROC_NOT_FOUND, status)
        }
    }
}

pub(crate) fn int_or_str(env: &Env<'_>, p: u32, wide: bool) -> IntOrStr {
    if p < 0x1_0000 {
        return IntOrStr::Int(p as u16);
    }
    let s = if wide {
        String::from_utf16_lossy(&wstr(env.cpu, p))
    } else {
        env.ext.get::<Win32>().codec.decode(&cstr(env.cpu, p))
    };
    match s.strip_prefix('#').and_then(|n| n.parse().ok()) {
        Some(n) => IntOrStr::Int(n),
        None => IntOrStr::Str(s),
    }
}

pub(crate) fn res_id(v: IntOrStr) -> ResId {
    match v {
        IntOrStr::Int(i) => ResId::Id(i),
        IntOrStr::Str(s) => ResId::Name(s),
    }
}

/// Guest address and size of a resource of module `h` (NULL = the exe).
pub(crate) fn find_resource_data(
    env: &Env<'_>,
    h: u32,
    ty: &ResId,
    name: &ResId,
) -> Option<(u32, u32)> {
    let m = adv_os::modules(env.ext).get(module_of(env, h)?);
    if let Some(raw) = &m.data_file {
        return raw.data(env.cpu, m.base, raw.find(env.cpu, m.base, ty, name, None)?);
    }
    let (_, data) = m.resources.as_ref()?.find(ty, name, None)?;
    resource_data(env, h, m.base + data.entry_rva)
}

fn find_resource(
    env: &mut Env<'_>,
    h: u32,
    name: u32,
    ty: u32,
    lang: Option<u16>,
    wide: bool,
) -> Outcome {
    let (name, ty) = (
        res_id(int_or_str(env, name, wide)),
        res_id(int_or_str(env, ty, wide)),
    );
    let Some(id) = module_of(env, h) else {
        set_last_error(env, err::MOD_NOT_FOUND);
        return Outcome::Return(0);
    };
    let m = adv_os::modules(env.ext).get(id);
    if let Some(raw) = &m.data_file {
        return match raw.find(env.cpu, m.base, &ty, &name, lang) {
            Some(entry) => Outcome::Return(entry),
            None => {
                set_last_error(env, err::RESOURCE_NAME_NOT_FOUND);
                Outcome::Return(0)
            }
        };
    }
    let found = m.resources.as_ref().and_then(|r| r.find(&ty, &name, lang));
    let Some((_, data)) = found else {
        log::debug!("FindResource({}, {name:?}, {ty:?}): not found", m.name);
        set_last_error(env, err::RESOURCE_NAME_NOT_FOUND);
        return Outcome::Return(0);
    };
    // HRSRC is an address in the owning image, not a separately allocated
    // handle. Repeated queries are stable and unload retires it with the image.
    Outcome::Return(m.base + data.entry_rva)
}

fn resource_data(env: &Env<'_>, module: u32, resource: u32) -> Option<(u32, u32)> {
    let image = adv_os::modules(env.ext).get(module_of(env, module)?);
    if let Some(raw) = &image.data_file {
        return raw.data(env.cpu, image.base, resource);
    }
    let entry = resource.checked_sub(image.base)?;
    if entry.checked_add(16)? > image.size {
        return None;
    }
    // Observe the live data entry, including intentional resource patches.
    let rva = env.cpu.read_u32(resource).ok()?;
    let size = env.cpu.read_u32(resource + 4).ok()?;
    if rva.checked_add(size)? > image.size {
        return None;
    }
    Some((image.base.checked_add(rva)?, size))
}

#[win32(kernel32, name = "LoadResource", level = implement)]
fn load_resource(env: &mut Env<'_>, module: Handle, resource: Handle) -> Handle {
    Handle(resource_data(env, module.0, resource.0).map_or(0, |(address, _)| address))
}

#[win32(kernel32, name = "SizeofResource", level = implement)]
fn sizeof_resource(env: &mut Env<'_>, module: Handle, resource: Handle) -> u32 {
    resource_data(env, module.0, resource.0).map_or(0, |(_, size)| size)
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(
        reg,
        notifications::INTERNAL,
        notifications::THREAD_START,
        2,
        notifications::thread_start,
    );
    stdcall(
        reg,
        notifications::INTERNAL,
        "startup_enter",
        0,
        notifications::startup_enter,
    );
    stdcall(
        reg,
        notifications::INTERNAL,
        "startup_leave",
        0,
        notifications::startup_leave,
    );
    stdcall(reg, K32, "LoadLibraryA", 1, |e| {
        let n = arg_a(e, 0);
        locked(e, move |e| load_library(e, n))
    });
    stdcall(reg, K32, "LoadLibraryW", 1, |e| {
        let n = arg_w(e, 0);
        locked(e, move |e| load_library(e, n))
    });
    reg.register(load_library_ex_a_ABI);
    reg.register(load_library_ex_w_ABI);
    reg.register(free_library_ABI);
    reg.register(free_library_and_exit_thread_ABI);
    stdcall(reg, K32, "DisableThreadLibraryCalls", 1, |e| {
        let handle = e.arg(0);
        locked(e, move |e| {
            let Some(id) = adv_os::modules(e.ext).by_handle(handle) else {
                set_last_error(e, err::MOD_NOT_FOUND);
                return Outcome::Return(0);
            };
            let module = adv_os::modules_mut(e.ext).get_mut(id);
            // The native fixture keeps notifications for a dynamically loaded
            // static-TLS DLL even though this call reports success.
            if module.lifetime.tls_index.is_none() {
                module.lifetime.thread_calls = false;
            }
            Outcome::Return(1)
        })
    });
    stdcall(reg, K32, "GetModuleHandleA", 1, |e| {
        let n = (e.arg(0) != 0).then(|| arg_a(e, 0));
        locked(e, move |e| get_module_handle(e, n))
    });
    stdcall(reg, K32, "GetModuleHandleW", 1, |e| {
        let n = (e.arg(0) != 0).then(|| arg_w(e, 0));
        locked(e, move |e| get_module_handle(e, n))
    });
    stdcall(reg, K32, "GetModuleHandleExA", 3, |e| {
        locked(e, |e| get_module_handle_ex(e, false))
    });
    stdcall(reg, K32, "GetModuleHandleExW", 3, |e| {
        locked(e, |e| get_module_handle_ex(e, true))
    });
    stdcall(reg, K32, "GetModuleFileNameA", 3, |e| {
        get_module_file_name(e, false)
    });
    stdcall(reg, K32, "GetModuleFileNameW", 3, |e| {
        get_module_file_name(e, true)
    });
    stdcall(reg, K32, "GetProcAddress", 2, |e| {
        locked(e, get_proc_address)
    });
    stdcall(reg, K32, "FindResourceA", 3, |e| {
        let (h, n, t) = (e.arg(0), e.arg(1), e.arg(2));
        find_resource(e, h, n, t, None, false)
    });
    stdcall(reg, K32, "FindResourceW", 3, |e| {
        let (h, n, t) = (e.arg(0), e.arg(1), e.arg(2));
        find_resource(e, h, n, t, None, true)
    });
    stdcall(reg, K32, "FindResourceExA", 4, |e| {
        let (h, t, n, l) = (e.arg(0), e.arg(1), e.arg(2), e.arg(3));
        find_resource(e, h, n, t, Some(l as u16), false)
    });
    reg.register(load_resource_ABI);
    stdcall(reg, K32, "LockResource", 1, |e| Outcome::Return(e.arg(0)));
    reg.register(sizeof_resource_ABI);
    stdcall(reg, K32, "FreeResource", 1, |_| Outcome::Return(0));
}
