//! Process status over guest process, Loader and section owners. No host process,
//! module, filesystem path or executable address is exposed through these APIs.
use crate::{
    Win32,
    kernel32::{child_process::process_target, module},
    util::set_last_error,
};
use adv_abi::{Registry, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
use adv_os::processes::{self, Error};
fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
fn visit<T: 'static>(
    env: &mut Env<'_>,
    handle: u32,
    access: u32,
    task: impl FnOnce(&mut Env<'_>) -> Result<T, Error> + 'static,
    complete: impl FnOnce(&mut Env<'_>, T) -> Outcome + 'static,
) -> Outcome {
    let target = match process_target(env, handle, access) {
        Ok(target) => target,
        Err(error) => return fail(env, error.code),
    };
    processes::visit(
        env,
        target.pid,
        None,
        task,
        move |env, result| match result {
            Ok(value) => complete(env, value),
            Err(error) => fail(env, error.code),
        },
    )
}
fn processes(env: &mut Env<'_>) -> Outcome {
    let (pointer, capacity, needed) = (env.arg(0), env.arg(1), env.arg(2));
    let mut pids = env
        .ext
        .try_get::<processes::Services>()
        .map(|service| {
            service
                .live_processes()
                .into_iter()
                .map(|(pid, _)| pid)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !pids.contains(&env.sys.pid) {
        pids.push(env.sys.pid);
    }
    pids.sort_unstable();
    pids.dedup();
    let count = (capacity / 4).min(pids.len() as u32);
    if check_range(env.cpu, needed, 4, Prot::WRITE).is_err()
        || check_range(env.cpu, pointer, count * 4, Prot::WRITE).is_err()
    {
        return fail(env, 998);
    }
    let bytes: Vec<_> = pids[..count as usize]
        .iter()
        .flat_map(|pid| pid.to_le_bytes())
        .collect();
    if count != 0 && env.cpu.write(pointer, &bytes).is_err() {
        return fail(env, 998);
    }
    let _ = env.cpu.write_u32(needed, count * 4); // EnumProcesses reports copied bytes, not the full required size.
    Outcome::Return(1)
}
fn module_info(env: &mut Env<'_>) -> Outcome {
    let (process, handle, pointer, length) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if length < 12 {
        return fail(env, 122);
    }
    if check_range(env.cpu, pointer, 12, Prot::WRITE).is_err() {
        return fail(env, 998);
    }
    visit(
        env,
        process,
        0x0410,
        move |target| {
            let id = module::module_of(target, handle)
                .ok_or_else(|| Error::new(126, "module not found"))?;
            let value = adv_os::modules(target.ext).get(id);
            if !value.executable_visible() || !value.lifetime.mapped {
                return Err(Error::new(126, "module not visible"));
            }
            Ok([value.base, value.size, value.entry])
        },
        move |env, info| {
            let bytes: Vec<_> = info.into_iter().flat_map(u32::to_le_bytes).collect();
            if env.cpu.write(pointer, &bytes).is_err() {
                return fail(env, 998);
            }
            Outcome::Return(1)
        },
    )
}
#[derive(Clone, Copy)]
enum TextMode {
    Truncate,
    TruncateError,
    Complete,
}
fn write_text(
    env: &mut Env<'_>,
    text: &str,
    pointer: u32,
    capacity: u32,
    wide: bool,
    mode: TextMode,
) -> Outcome {
    if capacity == 0 {
        return fail(env, 122);
    }
    let units: Vec<_> = text.encode_utf16().collect();
    let encoded = if wide {
        units
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect::<Vec<_>>()
    } else {
        env.ext.get::<Win32>().codec.from_utf16(0, &units).0
    };
    let width = if wide { 2 } else { 1 };
    let characters = encoded.len() / width;
    if matches!(mode, TextMode::Complete) && characters >= capacity as usize {
        return fail(env, 122);
    }
    let count = characters.min(capacity as usize - 1);
    let mut output = encoded[..count * width].to_vec();
    output.resize(output.len() + width, 0);
    if check_range(env.cpu, pointer, output.len() as u32, Prot::WRITE).is_err()
        || env.cpu.write(pointer, &output).is_err()
    {
        return fail(env, 998);
    }
    if characters >= capacity as usize {
        if matches!(mode, TextMode::TruncateError) {
            set_last_error(env, 122);
        }
        Outcome::Return(capacity)
    } else {
        Outcome::Return(count as u32)
    }
}
fn base_name(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (process, handle, pointer, capacity) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    visit(
        env,
        process,
        0x0410,
        move |target| {
            let id = module::module_of(target, handle)
                .ok_or_else(|| Error::new(126, "module not found"))?;
            let value = adv_os::modules(target.ext).get(id);
            if !value.executable_visible() || !value.lifetime.mapped {
                return Err(Error::new(126, "module not visible"));
            }
            let path = module::module_path(target, id)
                .map_err(|_| Error::new(998, "module path unreadable"))?;
            Ok(path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned())
        },
        move |env, name| write_text(env, &name, pointer, capacity, wide, TextMode::Truncate),
    )
}
fn native_path(env: &Env<'_>, path: &str) -> Result<String, Error> {
    let normalized = env.ext.get::<Win32>().vfs.normalize(path);
    let bytes = normalized.as_bytes();
    if bytes.get(1) != Some(&b':') {
        return Err(Error::new(3, "path has no DOS drive"));
    }
    let device = crate::kernel32::file::dos_device(env, bytes[0])
        .ok_or_else(|| Error::new(3, "DOS drive not found"))?;
    Ok(format!("{device}{}", &normalized[2..]))
}
fn image_name(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (process, pointer, capacity) = (env.arg(0), env.arg(1), env.arg(2));
    // Vista+ permits either query right. process_target tests all requested bits,
    // so explicitly choose the right actually granted by this handle.
    let access = if process_target(env, process, 0x1000).is_ok() {
        0x1000
    } else {
        0x400
    };
    visit(
        env,
        process,
        access,
        |target| native_path(target, &target.ext.get::<Win32>().exe_path()),
        move |env, path| write_text(env, &path, pointer, capacity, wide, TextMode::Complete),
    )
}
fn mapped_name(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (process, address, pointer, capacity) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    visit(
        env,
        process,
        0x0410,
        move |target| {
            let path = if let Some(id) = adv_os::modules(target.ext).containing(address) {
                let value = adv_os::modules(target.ext).get(id);
                if !value.lifetime.mapped {
                    return Err(Error::new(487, "image is unmapped"));
                }
                module::module_path(target, id)
                    .map_err(|_| Error::new(998, "module path unreadable"))?
            } else if let Some(raw) = adv_os::modules(target.ext).data_file_containing(address) {
                raw.path.clone()
            } else {
                crate::kernel32::file_mapping::mapped_path(target, address)
                    .ok_or_else(|| Error::new(1006, "address has no mapped file"))?
            };
            native_path(target, &path)
        },
        move |env, path| write_text(env, &path, pointer, capacity, wide, TextMode::TruncateError),
    )
}
pub(super) fn register(registry: &mut Registry) {
    for (name, argc, handler) in [
        ("EnumProcesses", 3, processes as adv_machine::Handler),
        ("GetModuleInformation", 4, module_info),
        ("GetModuleBaseNameA", 4, |env| base_name(env, false)),
        ("GetModuleBaseNameW", 4, |env| base_name(env, true)),
        ("GetProcessImageFileNameA", 3, |env| image_name(env, false)),
        ("GetProcessImageFileNameW", 3, |env| image_name(env, true)),
        ("GetMappedFileNameA", 4, |env| mapped_name(env, false)),
        ("GetMappedFileNameW", 4, |env| mapped_name(env, true)),
    ] {
        crate::stdcall(registry, "psapi.dll", name, argc, handler);
    }
    // Existing K32 front ends already visit the actual target process. Reuse
    // their handlers and generated signatures rather than duplicating their state.
    for name in [
        "EnumProcessModules",
        "EnumProcessModulesEx",
        "GetModuleFileNameExA",
        "GetModuleFileNameExW",
    ] {
        let function = registry
            .get("kernel32.dll", &format!("K32{name}"))
            .expect("registered K32 API");
        registry.add("psapi.dll", name, function.conv, function.handler);
    }
    let aliases: Vec<_> = registry
        .registered()
        .filter(|(module, _, _)| *module == "psapi.dll")
        .map(|(_, name, function)| (format!("K32{name}"), function))
        .collect();
    for (name, function) in aliases {
        if adv_abi::signature::lookup("kernel32.dll", &name).is_some() {
            registry.add("kernel32.dll", &name, function.conv, function.handler);
        }
    }
}
#[cfg(test)]
mod tests;
