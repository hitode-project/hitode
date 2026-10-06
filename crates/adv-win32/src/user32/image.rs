//! LoadImage fronts share guest paths, module resources and ordinary USER owners.
pub(crate) mod decode;

use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;
use adv_pe::ResId;

use super::{IconImage, user};
use crate::{
    Win32,
    util::{MAX_STR, set_last_error},
};

const ICON: u32 = 1;
const CURSOR: u32 = 2;
const FROM_FILE: u32 = 0x10;
const DEFAULT_SIZE: u32 = 0x40;
const SHARED: u32 = 0x8000;
const MAX_FILE: u64 = 128 * 1024 * 1024;

fn fail(env: &mut Env<'_>, error: u32) -> Handle {
    set_last_error(env, error);
    Handle(0)
}
fn unsupported(env: &mut Env<'_>, api: &str, reason: &str, error: u32) -> Handle {
    adv_abi::warn_unavailable(env, api, reason);
    fail(env, error)
}

fn name(env: &Env<'_>, pointer: u32, wide: bool) -> Result<String, u32> {
    if wide {
        WideStr(GuestPtr::new(pointer))
            .read(env.cpu, MAX_STR)
            .map(|text| String::from_utf16_lossy(&text))
            .map_err(|_| 998)
    } else {
        AnsiStr(GuestPtr::new(pointer))
            .read(env.cpu, MAX_STR)
            .map(|text| env.ext.get::<Win32>().codec.decode(&text))
            .map_err(|_| 998)
    }
}

pub(super) fn resource_id(env: &Env<'_>, pointer: u32, wide: bool) -> Result<ResId, u32> {
    if pointer < 0x10000 {
        return Ok(ResId::Id(pointer as u16));
    }
    if !wide && !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001) {
        return Err(50);
    }
    let text = name(env, pointer, wide)?;
    Ok(text
        .strip_prefix('#')
        .and_then(|n| n.parse::<u16>().ok())
        .map_or_else(|| ResId::Name(text), ResId::Id))
}

pub(super) fn load_cursor(env: &mut Env<'_>, instance: u32, pointer: u32, wide: bool) -> Handle {
    let api = if wide {
        "user32.dll!LoadCursorW"
    } else {
        "user32.dll!LoadCursorA"
    };
    let id = match resource_id(env, pointer, wide) {
        Ok(id) => id,
        Err(50) => {
            return unsupported(
                env,
                api,
                "ANSI cursor resource name code page has no supported decoder",
                50,
            );
        }
        Err(error) => return fail(env, error),
    };
    if instance == 0 {
        let ResId::Id(id) = id else {
            return fail(env, 1814);
        };
        if !system_cursor(u32::from(id)) {
            return fail(env, 1814);
        }
        let adv_machine::Outcome::Return(handle) =
            super::load_user_object(env, "cursor", 0, u32::from(id))
        else {
            unreachable!()
        };
        return Handle(handle);
    }
    cursor_metadata(env, instance, &id, false, api)
}

fn system_cursor(id: u32) -> bool {
    matches!(id, 32512..=32516 | 32640..=32646 | 32648..=32651)
}

fn cursor_metadata(
    env: &mut Env<'_>,
    instance: u32,
    id: &ResId,
    shared: bool,
    api: &str,
) -> Handle {
    // Preserve the original cursor resource identity, without claiming pixel
    // decoding. LoadCursor and LoadImage use one common USER metadata owner.
    if let Err(error) = resource_bytes(env, instance, 12, id, decode::MAX_CANDIDATE_BYTES) {
        return fail(env, error);
    }
    adv_abi::warn_unavailable(
        env,
        api,
        "module cursor retains the headless resource identity; pixel decoding/resizing is unsupported",
    );
    let key = if shared {
        format!("cursor shared {instance:#x} {id:?}")
    } else {
        format!("cursor {instance:#x} {id:?}")
    };
    if let Some(handle) = user(env).objects.find(&env.sys.objects, &key) {
        return Handle(handle);
    }
    let handle =
        env.ext
            .get_mut::<Win32>()
            .user
            .objects
            .create_metadata(&mut env.sys.objects, key, shared);
    handle.map_or_else(|| fail(env, 8), Handle)
}

fn file_bytes(env: &mut Env<'_>, path: &str, api: &str) -> Result<Vec<u8>, u32> {
    // The temporary owner participates in ordinary guest sharing checks and
    // coherent mapped-file reads; every successful open is closed on all paths.
    let handle = crate::kernel32::open_with_share(env, path, 0x80000000, 3, 7);
    if handle == crate::util::INVALID_HANDLE_VALUE {
        return Err(crate::util::last_error(env));
    }
    let result = (|| {
        let file = env.sys.objects.host_mut::<crate::FileObj>(handle).unwrap();
        let size = file.file.metadata().map_err(|_| 5_u32)?.len();
        if size > MAX_FILE {
            return Err(8);
        }
        let mut bytes = vec![0; size as usize];
        let got =
            crate::kernel32::read_coherent(file, &env.ext.get::<Win32>().vfs.sections, &mut bytes)
                .map_err(|_| 5_u32)?;
        bytes.truncate(got);
        Ok(bytes)
    })();
    env.sys.objects.close(handle);
    if result == Err(8) {
        adv_abi::warn_unavailable(
            env,
            api,
            "icon source exceeds the bounded 128MiB file profile",
        );
    }
    result
}

fn resource_bytes(
    env: &Env<'_>,
    instance: u32,
    ty: u16,
    id: &ResId,
    limit: usize,
) -> Result<Vec<u8>, u32> {
    let Some((pointer, size)) =
        crate::kernel32::find_resource_data(env, instance, &ResId::Id(ty), id)
    else {
        let modules = adv_os::modules(env.ext);
        let index = modules.by_handle(instance).ok_or(126_u32)?;
        let module = modules.get(index);
        return Err(
            if module.data_file.is_none()
                && module
                    .resources
                    .as_ref()
                    .is_none_or(|r| r.get(&ResId::Id(ty)).is_none())
            {
                1813
            } else {
                1814
            },
        );
    };
    if size as usize > limit {
        return Err(8);
    }
    check_range(env.cpu, pointer, size, Prot::READ).map_err(|_| 998_u32)?;
    let mut bytes = vec![0; size as usize];
    env.cpu.read(pointer, &mut bytes).map_err(|_| 998_u32)?;
    Ok(bytes)
}

fn resource_group(env: &Env<'_>, instance: u32, id: &ResId) -> Result<decode::Group, u32> {
    let raw = resource_bytes(env, instance, 14, id, decode::MAX_CANDIDATE_BYTES)?;
    if decode::word(&raw, 0) != Some(0) || decode::word(&raw, 2) != Some(1) {
        return Err(13);
    }
    let count = decode::word(&raw, 4).ok_or(13_u32)? as usize;
    if count == 0 || count > 4096 || raw.len() < 6 + count * 14 {
        return Err(13);
    }
    let mut images = Vec::new();
    let mut copied = 0_usize;
    for index in 0..count {
        let row = &raw[6 + index * 14..6 + (index + 1) * 14];
        let image_id = ResId::Id(decode::word(row, 12).ok_or(13_u32)?);
        let mut bytes = resource_bytes(
            env,
            instance,
            3,
            &image_id,
            decode::MAX_CANDIDATE_BYTES - copied,
        )?;
        let size = decode::dword(row, 8).ok_or(13_u32)? as usize;
        if size > bytes.len() {
            return Err(13);
        }
        copied = copied.checked_add(size).ok_or(8_u32)?;
        if copied > decode::MAX_CANDIDATE_BYTES {
            return Err(8);
        }
        bytes.truncate(size);
        images.push(decode::Candidate {
            width: if row[0] == 0 { 256 } else { u32::from(row[0]) },
            height: if row[1] == 0 { 256 } else { u32::from(row[1]) },
            bits: decode::word(row, 6).ok_or(13_u32)?,
            bytes,
        });
    }
    Ok(decode::Group {
        id: Some(id.clone()),
        images,
    })
}

fn publish(env: &mut Env<'_>, key: String, image: IconImage, shared: bool) -> Handle {
    let handle = env.ext.get_mut::<Win32>().user.objects.create_image(
        &mut env.sys.objects,
        key,
        image,
        shared,
    );
    handle.map_or_else(|| fail(env, 8), Handle)
}

#[allow(clippy::too_many_arguments)]
fn load(
    env: &mut Env<'_>,
    instance: u32,
    pointer: u32,
    ty: u32,
    cx: i32,
    cy: i32,
    flags: u32,
    wide: bool,
) -> Handle {
    let api = if wide {
        "user32.dll!LoadImageW"
    } else {
        "user32.dll!LoadImageA"
    };
    if cx < 0 || cy < 0 || ty > CURSOR {
        return fail(env, 87);
    }
    if flags & !(FROM_FILE | DEFAULT_SIZE | SHARED) != 0 {
        return unsupported(
            env,
            api,
            "image conversion/palette flags are unsupported",
            50,
        );
    }
    if cx > 1024 || cy > 1024 {
        return unsupported(
            env,
            api,
            "image dimensions exceed the bounded 1024-pixel profile",
            8,
        );
    }
    if ty == 0 {
        return unsupported(env, api, "bitmap LoadImage is unsupported", 50);
    }
    let width = if cx == 0 && flags & DEFAULT_SIZE != 0 {
        32
    } else {
        cx as u32
    };
    let height = if cy == 0 && flags & DEFAULT_SIZE != 0 {
        32
    } else {
        cy as u32
    };
    if pointer >= 0x10000
        && !wide
        && !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001)
    {
        return unsupported(
            env,
            api,
            "ANSI image name code page has no supported decoder",
            50,
        );
    }
    if flags & FROM_FILE != 0 {
        // Owned x86 SDK oracle: LR_LOADFROMFILE ignores even an invalid hInst.
        // LR_SHARED is ignored for files: every successful call owns its image.
        if pointer < 0x10000 {
            return fail(env, 87);
        }
        if ty != ICON {
            return unsupported(
                env,
                api,
                "cursor/animated cursor file decoding is unsupported",
                50,
            );
        }
        let path = match name(env, pointer, wide) {
            Ok(path) => path,
            Err(error) => return fail(env, error),
        };
        let bytes = match file_bytes(env, &path, api) {
            Ok(bytes) => bytes,
            Err(error) => return fail(env, error),
        };
        let Some(groups) = decode::ico_groups(&bytes) else {
            // Native malformed ICO failure can leave LastError zero.
            return unsupported(
                env,
                api,
                "ICO directory is malformed or exceeds the bounded 16MiB candidate profile",
                0,
            );
        };
        let Some(image) = decode::image_size(&groups[0], width, height) else {
            return unsupported(
                env,
                api,
                "ICO image encoding is unsupported or malformed (classic uncompressed DIB required)",
                0,
            );
        };
        return publish(
            env,
            format!("icon file {path} {}x{}", image.width, image.height),
            image,
            false,
        );
    }
    // Keep the existing virtual system-resource identities; nonshared system
    // requests fail as on the native oracle. They are process-owned stock data.
    if instance == 0 {
        if flags & SHARED == 0 {
            return fail(env, 1813);
        }
        if pointer >= 0x10000 {
            return fail(env, 1814);
        }
        let valid = if ty == ICON {
            (32512..=32518).contains(&pointer)
        } else {
            system_cursor(pointer)
        };
        if !valid {
            return fail(env, 1814);
        }
        let adv_machine::Outcome::Return(handle) =
            super::load_user_object(env, if ty == ICON { "icon" } else { "cursor" }, 0, pointer)
        else {
            unreachable!()
        };
        return Handle(handle);
    }
    let Some(index) = env
        .ext
        .try_get::<adv_os::Modules>()
        .and_then(|m| m.by_handle(instance))
    else {
        return fail(env, 126);
    };
    let id = match resource_id(env, pointer, wide) {
        Ok(id) => id,
        Err(error) => return fail(env, error),
    };
    if ty == CURSOR {
        return cursor_metadata(env, instance, &id, flags & SHARED != 0, api);
    }
    let cache_id = match &id {
        ResId::Id(id) => format!("#{id}"),
        ResId::Name(name) => name.to_lowercase(),
    };
    let shared = flags & SHARED != 0;
    let key = format!(
        "icon {}resource module {index} {instance:#x} {cache_id}",
        if shared { "shared " } else { "" }
    );
    if shared && let Some(handle) = user(env).objects.find(&env.sys.objects, &key) {
        return Handle(handle);
    }
    let group = match resource_group(env, instance, &id) {
        Ok(group) => group,
        Err(8) => {
            return unsupported(
                env,
                api,
                "resource icon exceeds the bounded 16MiB candidate profile",
                8,
            );
        }
        Err(error) => return fail(env, error),
    };
    let Some(image) = decode::image_size(&group, width, height) else {
        return unsupported(
            env,
            api,
            "resource icon encoding is unsupported or malformed (classic uncompressed DIB required)",
            13,
        );
    };
    publish(env, key, image, shared)
}

#[win32(user32, name = "LoadImageA", level = implement)]
fn load_a(
    env: &mut Env<'_>,
    instance: Handle,
    image: AnsiStr,
    ty: u32,
    cx: i32,
    cy: i32,
    flags: u32,
) -> Handle {
    load(env, instance.0, image.0.addr(), ty, cx, cy, flags, false)
}
#[win32(user32, name = "LoadImageW", level = implement)]
fn load_w(
    env: &mut Env<'_>,
    instance: Handle,
    image: WideStr,
    ty: u32,
    cx: i32,
    cy: i32,
    flags: u32,
) -> Handle {
    load(env, instance.0, image.0.addr(), ty, cx, cy, flags, true)
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(load_a_ABI);
    registry.register(load_w_ABI);
}

#[cfg(test)]
mod tests;
