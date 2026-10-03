//! Modules and resources: LoadLibrary, GetProcAddress, GetModuleHandle/FileName,
//! FindResource & co.

use std::collections::VecDeque;

use adv_abi::Registry;
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, InitCall, Loader, ModuleId, ModuleKind, Outcome};
use adv_pe::{ImportName, ResId};

use super::K32;
use crate::util::{IntOrStr, cstr, err, put_cstr, put_wstr, set_last_error, wstr};
use crate::{Win32, stdcall};

/// Run DllMain/TLS initializers of newly loaded modules, then return `ret`.
fn run_inits(mut inits: VecDeque<InitCall>, ret: u32) -> Outcome {
    match inits.pop_front() {
        None => Outcome::Return(ret),
        Some(c) => {
            log::debug!("LoadLibrary init: {}", c.what);
            Outcome::call(c.target, c.args, move |_, _| run_inits(inits, ret))
        }
    }
}

fn load_library(env: &mut Env<'_>, name: String) -> Outcome {
    let w = env.ext.get::<Win32>();
    let mut l = Loader::new(&mut *env.cpu, &mut *env.sys, &w.vfs);
    let res = l.load(&name);
    let inits: VecDeque<InitCall> = std::mem::take(&mut l.inits).into();
    drop(l);
    match res {
        Ok(id) => {
            let base = env.sys.modules.get(id).base;
            log::info!("LoadLibrary({name}) = {base:#010x}");
            run_inits(inits, base)
        }
        Err(e) => {
            log::warn!("LoadLibrary({name}) failed: {e}");
            set_last_error(env, err::MOD_NOT_FOUND);
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
        return env.sys.modules.by_handle(exe);
    }
    env.sys.modules.by_handle(h)
}

fn get_module_handle(env: &mut Env<'_>, name: Option<String>) -> Outcome {
    let id = match &name {
        None => module_of(env, 0),
        Some(n) => env.sys.modules.by_name(n),
    };
    match id {
        Some(id) => Outcome::Return(env.sys.modules.get(id).base),
        None => {
            log::debug!("GetModuleHandle({name:?}): not loaded");
            set_last_error(env, err::MOD_NOT_FOUND);
            Outcome::Return(0)
        }
    }
}

/// GetModuleHandleEx(flags, name, out): FROM_ADDRESS (4) takes an address inside the module
/// instead of a name. Reference counts are not kept, so the pin/unchanged flags don't matter.
fn get_module_handle_ex(env: &mut Env<'_>, wide: bool) -> Outcome {
    const FROM_ADDRESS: u32 = 4;
    let (flags, name, out) = (env.arg(0), env.arg(1), env.arg(2));
    let base = if flags & FROM_ADDRESS != 0 {
        env.sys
            .modules
            .iter()
            .find(|(_, m)| m.contains(name))
            .map(|(_, m)| m.base)
    } else if name == 0 {
        module_of(env, 0).map(|id| env.sys.modules.get(id).base)
    } else {
        let n = if wide { arg_w(env, 1) } else { arg_a(env, 1) };
        env.sys
            .modules
            .by_name(&n)
            .map(|id| env.sys.modules.get(id).base)
    };
    if out != 0 {
        let _ = env.cpu.write_u32(out, base.unwrap_or(0));
    }
    match base {
        Some(_) => Outcome::Return(1),
        None => {
            set_last_error(env, err::MOD_NOT_FOUND);
            Outcome::Return(0)
        }
    }
}

/// Guest path of a module.
fn module_path(env: &Env<'_>, id: ModuleId) -> String {
    let w = env.ext.get::<Win32>();
    let m = env.sys.modules.get(id);
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
    let m = env.sys.modules.get(id);
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
    let w = env.ext.get::<Win32>();
    let mut l = Loader::new(&mut *env.cpu, &mut *env.sys, &w.vfs);
    let r = l.resolve(id, &sym);
    drop(l);
    env.sys.trace.resolution(
        caller,
        &env.sys.modules.get(id).name,
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
    let m = env.sys.modules.get(module_of(env, h)?);
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
    let m = env.sys.modules.get(id);
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
        load_library(e, n)
    });
    stdcall(reg, K32, "LoadLibraryW", 1, |e| {
        let n = arg_w(e, 0);
        load_library(e, n)
    });
    stdcall(reg, K32, "LoadLibraryExA", 3, |e| {
        let n = arg_a(e, 0);
        load_library(e, n)
    });
    stdcall(reg, K32, "LoadLibraryExW", 3, |e| {
        let n = arg_w(e, 0);
        load_library(e, n)
    });
    stdcall(reg, K32, "FreeLibrary", 1, |_| Outcome::Return(1));
    stdcall(reg, K32, "DisableThreadLibraryCalls", 1, |_| {
        Outcome::Return(1)
    });
    stdcall(reg, K32, "GetModuleHandleA", 1, |e| {
        let n = (e.arg(0) != 0).then(|| arg_a(e, 0));
        get_module_handle(e, n)
    });
    stdcall(reg, K32, "GetModuleHandleW", 1, |e| {
        let n = (e.arg(0) != 0).then(|| arg_w(e, 0));
        get_module_handle(e, n)
    });
    stdcall(reg, K32, "GetModuleHandleExA", 3, |e| {
        get_module_handle_ex(e, false)
    });
    stdcall(reg, K32, "GetModuleHandleExW", 3, |e| {
        get_module_handle_ex(e, true)
    });
    stdcall(reg, K32, "GetModuleFileNameA", 3, |e| {
        get_module_file_name(e, false)
    });
    stdcall(reg, K32, "GetModuleFileNameW", 3, |e| {
        get_module_file_name(e, true)
    });
    stdcall(reg, K32, "GetProcAddress", 2, get_proc_address);
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
