//! Modules and resources: LoadLibrary, GetProcAddress, GetModuleHandle/FileName,
//! FindResource & co.

use std::collections::VecDeque;

use adv_abi::Registry;
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, InitCall, Object, Outcome, Wait};
use adv_os::{Loader, ModuleId, ModuleKind, Modules};
use adv_pe::{ImportName, ResId};

use super::K32;
use crate::util::{IntOrStr, cstr, err, put_cstr, put_wstr, set_last_error, wstr};
use crate::{Win32, stdcall};

fn release_lock(env: &mut Env<'_>, handle: u32, outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Call { target, args, then } => Outcome::call(target, args, move |env, resume| {
            let next = then(env, resume);
            release_lock(env, handle, next)
        }),
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
                debug_assert_eq!(*owner, Some(env.sys.current));
                *recursion -= 1;
                if *recursion == 0 {
                    *owner = None;
                }
            }
            terminal
        }
    }
}

fn locked(env: &mut Env<'_>, operation: impl FnOnce(&mut Env<'_>) -> Outcome + 'static) -> Outcome {
    let handle = match env.ext.get::<Win32>().loader_lock {
        Some(handle) => handle,
        None => {
            let handle = env.sys.objects.insert(Object::Mutex {
                owner: None,
                recursion: 0,
            });
            if handle == 0 {
                set_last_error(env, err::NOT_ENOUGH_MEMORY);
                return Outcome::Return(0);
            }
            env.ext.get_mut::<Win32>().loader_lock = Some(handle);
            handle
        }
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
    let module = adv_os::modules(env.ext).get(id);
    let (base, size) = (module.base, module.size);
    env.ext
        .get_mut::<Win32>()
        .resources
        .retain(|_, (address, _)| address.wrapping_sub(base) >= size);
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

/// Run dependency initializers before the requested image. Only guest returns
/// enter this continuation; a failed DllMain unwinds the acquired dependency graph.
fn run_inits(
    env: &mut Env<'_>,
    mut inits: VecDeque<InitCall>,
    root: ModuleId,
    ret: u32,
) -> Outcome {
    match inits.pop_front() {
        None => Outcome::Return(ret),
        Some(c) => {
            log::debug!("LoadLibrary init: {}", c.what);
            let module = adv_os::modules(env.ext).by_handle(c.args[0]).unwrap();
            let is_dllmain = adv_os::modules(env.ext).get(module).entry == c.target;
            adv_os::modules_mut(env.ext)
                .get_mut(module)
                .lifetime
                .attach_started = true;
            Outcome::call(c.target, c.args, move |env, resume| {
                if is_dllmain && resume.value() == 0 {
                    let ids = match adv_os::modules_mut(env.ext).release(env.cpu, root) {
                        Ok(ids) => ids,
                        Err(error) => return Outcome::Fail(error.to_string()),
                    };
                    unload(env, ids, 0, Some(1114))
                } else {
                    run_inits(env, inits, root, ret)
                }
            })
        }
    }
}

fn load_library(env: &mut Env<'_>, name: String) -> Outcome {
    let (w, modules) = env.ext.get2_mut::<Win32, Modules>();
    let mut l = Loader::new(&mut *env.cpu, &mut *env.sys, modules, &w.vfs);
    let res = l.load(&name);
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
            run_inits(env, inits, id, base)
        }
        Err(e) => {
            log::warn!("LoadLibrary({name}) failed: {e}");
            let code = match e {
                adv_os::LoadError::Pe { .. } | adv_os::LoadError::NoRelocs(..) => 193,
                adv_os::LoadError::NoSpace(..)
                | adv_os::LoadError::TooManyHostModules
                | adv_os::LoadError::Mem(_) => err::NOT_ENOUGH_MEMORY,
                adv_os::LoadError::MissingExport { .. } => err::PROC_NOT_FOUND,
                adv_os::LoadError::NotFound(_) => err::MOD_NOT_FOUND,
            };
            set_last_error(env, code);
            Outcome::Return(0)
        }
    }
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
        adv_os::modules(env.ext)
            .iter()
            .find(|(_, m)| m.contains(name))
            .map(|(_, m)| m.base)
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

/// Guest path of a module.
fn module_path(env: &Env<'_>, id: ModuleId) -> String {
    let w = env.ext.get::<Win32>();
    let m = adv_os::modules(env.ext).get(id);
    match m.kind {
        ModuleKind::Host => format!("C:\\Windows\\system32\\{}", m.name),
        ModuleKind::Image if !m.is_dll => w.exe_path(),
        ModuleKind::Image => w.vfs.normalize(&m.path),
    }
}

fn get_module_file_name(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (h, buf, cap) = (env.arg(0), env.arg(1), env.arg(2));
    let Some(id) = module_of(env, h) else {
        set_last_error(env, err::MOD_NOT_FOUND);
        return Outcome::Return(0);
    };
    let path = module_path(env, id);
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
    let Some(id) = module_of(env, h) else {
        env.sys
            .trace
            .resolution(caller, "<invalid-hmodule>", &sym, 0);
        set_last_error(env, err::MOD_NOT_FOUND);
        return Outcome::Return(0);
    };
    let m = adv_os::modules(env.ext).get(id);
    if m.kind == ModuleKind::Host
        && env
            .sys
            .thunks
            .resolver()
            .host_fn(&m.name, &sym.to_string())
            .is_none()
    {
        // Unimplemented host function: report "not found" so feature probes fall back.
        log::info!("GetProcAddress({}, {sym}) = NULL (not implemented)", m.name);
        env.sys.trace.resolution(caller, &m.name, &sym, 0);
        set_last_error(env, err::PROC_NOT_FOUND);
        return Outcome::Return(0);
    }
    let (w, modules) = env.ext.get2_mut::<Win32, Modules>();
    let mut l = Loader::new(&mut *env.cpu, &mut *env.sys, modules, &w.vfs);
    let r = l.resolve(id, &sym);
    drop(l);
    env.sys.trace.resolution(
        caller,
        &adv_os::modules(env.ext).get(id).name,
        &sym,
        r.as_ref().copied().unwrap_or(0),
    );
    match r {
        Ok(a) => Outcome::Return(a),
        Err(e) => {
            log::info!("GetProcAddress: {e}");
            set_last_error(env, err::PROC_NOT_FOUND);
            Outcome::Return(0)
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
    let (_, data) = m.resources.as_ref()?.find(ty, name, None)?;
    Some((m.base + data.rva, data.size))
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
    let found = m.resources.as_ref().and_then(|r| r.find(&ty, &name, lang));
    let Some((_, data)) = found else {
        log::debug!("FindResource({}, {name:?}, {ty:?}): not found", m.name);
        set_last_error(env, err::RESOURCE_NAME_NOT_FOUND);
        return Outcome::Return(0);
    };
    let addr = m.base + data.rva;
    let w = env.ext.get_mut::<Win32>();
    let h = w.next_hrsrc;
    w.next_hrsrc += 0x10;
    w.resources.insert(h, (addr, data.size));
    Outcome::Return(h)
}

fn load_resource(env: &mut Env<'_>) -> Outcome {
    let r = env.arg(1);
    Outcome::Return(
        env.ext
            .get::<Win32>()
            .resources
            .get(&r)
            .map_or(0, |&(a, _)| a),
    )
}

fn sizeof_resource(env: &mut Env<'_>) -> Outcome {
    let r = env.arg(1);
    Outcome::Return(
        env.ext
            .get::<Win32>()
            .resources
            .get(&r)
            .map_or(0, |&(_, s)| s),
    )
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "LoadLibraryA", 1, |e| {
        let n = arg_a(e, 0);
        locked(e, move |e| load_library(e, n))
    });
    stdcall(reg, K32, "LoadLibraryW", 1, |e| {
        let n = arg_w(e, 0);
        locked(e, move |e| load_library(e, n))
    });
    stdcall(reg, K32, "LoadLibraryExA", 3, |e| {
        let n = arg_a(e, 0);
        locked(e, move |e| load_library(e, n))
    });
    stdcall(reg, K32, "LoadLibraryExW", 3, |e| {
        let n = arg_w(e, 0);
        locked(e, move |e| load_library(e, n))
    });
    stdcall(reg, K32, "FreeLibrary", 1, |e| {
        let handle = e.arg(0);
        locked(e, move |e| {
            let Some(id) = adv_os::modules(e.ext).by_handle(handle) else {
                set_last_error(e, err::MOD_NOT_FOUND);
                return Outcome::Return(0);
            };
            match adv_os::modules_mut(e.ext).release(e.cpu, id) {
                Ok(ids) => unload(e, ids, 1, None),
                Err(error) => Outcome::Fail(error.to_string()),
            }
        })
    });
    stdcall(reg, K32, "DisableThreadLibraryCalls", 1, |e| {
        let handle = e.arg(0);
        locked(e, move |e| {
            let Some(id) = adv_os::modules(e.ext).by_handle(handle) else {
                set_last_error(e, err::MOD_NOT_FOUND);
                return Outcome::Return(0);
            };
            adv_os::modules_mut(e.ext).get_mut(id).lifetime.thread_calls = false;
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
    stdcall(reg, K32, "LoadResource", 2, load_resource);
    stdcall(reg, K32, "LockResource", 1, |e| Outcome::Return(e.arg(0)));
    stdcall(reg, K32, "SizeofResource", 2, sizeof_resource);
    stdcall(reg, K32, "FreeResource", 1, |_| Outcome::Return(0));
}
