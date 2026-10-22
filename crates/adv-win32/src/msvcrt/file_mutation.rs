//! CRT path mutations call the existing live Windows file owner. UCRT narrow
//! paths use the existing codec and heap to retain its wide Windows boundary.
use super::*;
use adv_abi::{AnsiStr, GuestPtr, guest::memory_failure};

pub(super) fn register(registry: &mut Registry) {
    versions::register(
        registry,
        &[
            ("remove", call),
            ("_unlink", call),
            ("_wremove", call),
            ("_wunlink", call),
            ("rename", call),
            ("_wrename", call),
            ("_o_remove", call),
            ("_o__unlink", call),
            ("_o__wremove", call),
            ("_o__wunlink", call),
            ("_o_rename", call),
            ("_o__wrename", call),
        ],
    );
}
fn returned(env: &mut Env<'_>, value: u32, profile: versions::Profile) -> Outcome {
    if value != 0 {
        return Outcome::Return(0);
    }
    let code = crate::util::last_error(env);
    // Legacy wrappers test GetLastError itself after a false result; UCRT
    // reports failure even if a replacement provider leaves error zero.
    if code == 0 && profile.zero_file_error_success {
        return Outcome::Return(0);
    }
    errors::map_dos(env, code, u32::MAX)
}
fn invoke(
    env: &mut Env<'_>,
    name: &'static str,
    mut args: Vec<u32>,
    rename: bool,
    profile: versions::Profile,
    buffers: Vec<u32>,
) -> Outcome {
    let entry = match crate::native::public_entry(env, "kernel32.dll", name) {
        Ok(entry) => entry,
        Err(reason) => {
            return finish(
                env,
                buffers,
                Outcome::Fail(format!("CRT live {name} resolution: {reason}")),
            );
        }
    };
    if rename && profile.rename_copy_allowed {
        args.push(2);
    } // MOVEFILE_COPY_ALLOWED.
    Outcome::call(entry, args, move |env, result| {
        let result = returned(env, result.value(), profile);
        finish(env, buffers, result)
    })
}
fn finish(env: &mut Env<'_>, mut buffers: Vec<u32>, result: Outcome) -> Outcome {
    match buffers.pop() {
        Some(buffer) => crate::native_heap::free_then(
            env,
            buffer,
            Box::new(move |env, _| finish(env, buffers, result)),
        ),
        None => result,
    }
}
fn convert_next(
    env: &mut Env<'_>,
    name: &'static str,
    pointers: Vec<u32>,
    mut args: Vec<u32>,
    mut buffers: Vec<u32>,
    rename: bool,
    profile: versions::Profile,
) -> Outcome {
    let Some(&pointer) = pointers.get(args.len()) else {
        return invoke(env, name, args, rename, profile, buffers);
    };
    // Native conversion preserves NULL and allocates each nonnull path before
    // examining the next one. This also preserves allocation/error precedence.
    if pointer == 0 {
        args.push(0);
        return convert_next(env, name, pointers, args, buffers, rename, profile);
    }
    let input = match AnsiStr(GuestPtr::new(pointer)).read(env.cpu, 32768) {
        Ok(input) => input,
        Err(fault) => return finish(env, buffers, memory_failure(fault)),
    };
    let codec = env.ext.get::<crate::Win32>().codec;
    let codepage = if locale::formatting_codepage(env) == 65001 {
        65001
    } else {
        codec.acp
    };
    let (units, invalid) = codec.to_utf16_checked(codepage, &input);
    if invalid {
        crate::util::set_last_error(env, 1113);
        let result = errors::map_dos(env, 1113, u32::MAX);
        return finish(env, buffers, result);
    }
    let bytes: Vec<_> = units
        .into_iter()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    crate::native_heap::allocate_then(
        env,
        bytes.len() as u32,
        false,
        Box::new(move |env, buffer| {
            if buffer == 0 {
                let errno = floating::errno_address(env);
                let result = if errno.is_null() {
                    crate::kernel32::exit_process(env, 255)
                } else {
                    match errno.write(env.cpu, 12) {
                        Ok(()) => Outcome::Return(u32::MAX),
                        Err(fault) => memory_failure(fault),
                    }
                };
                return finish(env, buffers, result);
            }
            buffers.push(buffer);
            if let Err(fault) = env.cpu.write(buffer, &bytes) {
                return finish(env, buffers, memory_failure(fault));
            }
            args.push(buffer);
            convert_next(env, name, pointers, args, buffers, rename, profile)
        }),
    )
}

fn call(env: &mut Env<'_>) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let profile = versions::profile(&thunk.module);
    // The UCRT _o_ guard selects the configured single application owner.
    let name = thunk.name.strip_prefix("_o_").unwrap_or(&thunk.name);
    let input_wide = name.starts_with("_w");
    let wide = input_wide || profile.unicode_file_paths;
    let rename = name.ends_with("rename");
    let name = match (rename, wide, profile.rename_copy_allowed) {
        (false, false, _) => "DeleteFileA",
        (false, true, _) => "DeleteFileW",
        (true, false, false) => "MoveFileA",
        (true, true, false) => "MoveFileW",
        (true, false, true) => "MoveFileExA",
        (true, true, true) => "MoveFileExW",
    };
    let mut args = vec![env.arg(0)];
    if rename {
        args.push(env.arg(1));
    }
    if input_wide || !profile.unicode_file_paths {
        invoke(env, name, args, rename, profile, Vec::new())
    } else {
        convert_next(env, name, args, Vec::new(), Vec::new(), rename, profile)
    }
}
