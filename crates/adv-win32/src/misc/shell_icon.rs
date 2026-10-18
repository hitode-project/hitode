//! Shell extraction reads guest files and creates ordinary, owned USER icons.
//! PE group resources and classic ICO DIBs share the same checked pixel decoder.
use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;
use adv_pe::ResId;

use crate::{
    Win32,
    user32::image::{
        decode::{ico_groups, image, pe_groups},
        file_bytes,
    },
    util::{MAX_STR, last_error, set_last_error},
};

const ERROR_NOACCESS: u32 = 998;

fn fail(env: &mut Env<'_>, error: u32) -> u32 {
    set_last_error(env, error);
    u32::MAX
}

fn extract(
    env: &mut Env<'_>,
    path: Result<String, u32>,
    index: i32,
    large: u32,
    small: u32,
    count: u32,
    api: &str,
) -> u32 {
    let query = index == -1 && large == 0 && small == 0;
    // Native rejects a zero extraction count without altering caller outputs or
    // LastError, while its -1/NULL count query ignores nIcons.
    if !query && count == 0 {
        return u32::MAX;
    }
    let path = match path {
        Ok(path) => path,
        Err(error) => return fail(env, error),
    };
    // LoadImage and shell extraction use one temporary FileObj, including guest
    // sharing checks and dirty mapped-file content. Its open must not reset the
    // extraction front's otherwise unspecified success LastError.
    let saved_error = last_error(env);
    let bytes = match file_bytes(env, &path, api) {
        Ok(bytes) => bytes,
        Err(2) if query => {
            set_last_error(env, 2);
            return 0;
        }
        Err(error) => return fail(env, error),
    };
    set_last_error(env, saved_error);
    let portable_executable = bytes.starts_with(b"MZ");
    let groups = if portable_executable {
        pe_groups(&bytes)
    } else {
        ico_groups(&bytes)
    }
    .unwrap_or_default();
    if query || (!portable_executable && large == 0 && small == 0) {
        return groups.len() as u32;
    }
    if large == 0 && small == 0 {
        return 0;
    }
    let Some(length) = count.checked_mul(4) else {
        return fail(env, ERROR_NOACCESS);
    };
    if [large, small]
        .into_iter()
        .any(|address| address != 0 && check_range(env.cpu, address, length, Prot::WRITE).is_err())
    {
        return fail(env, ERROR_NOACCESS);
    }
    // ICO's native negative-index path is a count query, even with output arrays;
    // it clears only the first slot and creates no HICON.
    if !portable_executable && index == -1 {
        for address in [large, small] {
            if address != 0 && env.cpu.write_u32(address, 0).is_err() {
                return fail(env, ERROR_NOACCESS);
            }
        }
        return groups.len() as u32;
    }
    let zero = [0; 4096];
    for address in [large, small].into_iter().filter(|&address| address != 0) {
        let mut offset = 0;
        while offset < length {
            let size = (length - offset).min(zero.len() as u32);
            if env
                .cpu
                .write(address + offset, &zero[..size as usize])
                .is_err()
            {
                return fail(env, ERROR_NOACCESS);
            }
            offset += size;
        }
    }
    let start = if index < 0 {
        let Some(id) = index.checked_neg().and_then(|id| u16::try_from(id).ok()) else {
            return 0;
        };
        let Some(start) = groups
            .iter()
            .position(|group| group.id == Some(ResId::Id(id)))
        else {
            return 0;
        };
        start
    } else {
        index as usize
    };
    let available = groups.len().saturating_sub(start).min(count as usize);
    let mut extracted = 0;
    for group in groups.iter().skip(start).take(available) {
        let mut handles = [0; 2];
        for (which, (address, size)) in [(large, 32), (small, 16)].into_iter().enumerate() {
            if address == 0 {
                continue;
            }
            let Some(image) = image(group, size) else {
                continue;
            };
            let description = format!("icon extracted {path} {:?} {size}x{size}", group.id);
            let Some(handle) = env.ext.get_mut::<Win32>().user.objects.create_icon(
                &mut env.sys.objects,
                description,
                image,
            ) else {
                for handle in handles {
                    env.ext
                        .get_mut::<Win32>()
                        .user
                        .objects
                        .destroy_icon(&mut env.sys.objects, handle);
                }
                return if extracted == 0 {
                    fail(env, 8)
                } else {
                    extracted
                };
            };
            handles[which] = handle;
        }
        for (address, handle) in [large, small].into_iter().zip(handles) {
            if address != 0 && env.cpu.write_u32(address + extracted * 4, handle).is_err() {
                for handle in handles {
                    env.ext
                        .get_mut::<Win32>()
                        .user
                        .objects
                        .destroy_icon(&mut env.sys.objects, handle);
                }
                return if extracted == 0 {
                    fail(env, ERROR_NOACCESS)
                } else {
                    extracted
                };
            }
        }
        if handles == [0; 2] {
            break;
        }
        extracted += 1;
    }
    // PE's dual-size path reports actual HICON count when the complete request
    // succeeds; an exhausted group enumeration reports the available group count.
    // ICO extraction reports logical icon count regardless of the two sizes.
    if portable_executable && large != 0 && small != 0 && extracted == count {
        extracted * 2
    } else {
        extracted
    }
}

#[win32(shell32, name = "ExtractIconExA", level = implement)]
fn extract_a(
    env: &mut Env<'_>,
    file: AnsiStr,
    index: i32,
    large: GuestPtr<Handle>,
    small: GuestPtr<Handle>,
    count: u32,
) -> u32 {
    let path = file
        .read(env.cpu, MAX_STR)
        .map(|bytes| env.ext.get::<Win32>().codec.decode(&bytes))
        .map_err(|_| ERROR_NOACCESS);
    extract(
        env,
        path,
        index,
        large.addr(),
        small.addr(),
        count,
        "shell32.dll!ExtractIconExA",
    )
}
#[win32(shell32, name = "ExtractIconExW", level = implement)]
fn extract_w(
    env: &mut Env<'_>,
    file: WideStr,
    index: i32,
    large: GuestPtr<Handle>,
    small: GuestPtr<Handle>,
    count: u32,
) -> u32 {
    let path = file
        .read(env.cpu, MAX_STR)
        .map(|units| String::from_utf16_lossy(&units))
        .map_err(|_| ERROR_NOACCESS);
    extract(
        env,
        path,
        index,
        large.addr(),
        small.addr(),
        count,
        "shell32.dll!ExtractIconExW",
    )
}

fn associated_fail(env: &mut Env<'_>, error: u32) -> Handle {
    set_last_error(env, error);
    Handle(0)
}

fn associated_unavailable(env: &mut Env<'_>, api: &str, reason: &str, error: u32) -> Handle {
    adv_abi::warn_unavailable(env, api, reason);
    associated_fail(env, error)
}

fn associated(env: &mut Env<'_>, path: u32, index: GuestPtr<u16>, wide: bool) -> Handle {
    let api = if wide {
        "shell32.dll!ExtractAssociatedIconW"
    } else {
        "shell32.dll!ExtractAssociatedIconA"
    };
    if !wide && !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001) {
        return associated_unavailable(
            env,
            api,
            "ANSI icon path code page has no supported decoder",
            50,
        );
    }
    let decoded = if wide {
        WideStr(GuestPtr::new(path))
            .read(env.cpu, MAX_STR)
            .map_err(|_| ERROR_NOACCESS)
            .and_then(|units| {
                let bytes = (units.len() as u32 + 1) * 2;
                String::from_utf16(&units)
                    .map(|text| (text, bytes))
                    .map_err(|_| 1113)
            })
    } else {
        AnsiStr(GuestPtr::new(path))
            .read(env.cpu, MAX_STR)
            .map(|bytes| {
                (
                    env.ext.get::<Win32>().codec.decode(&bytes),
                    bytes.len() as u32 + 1,
                )
            })
            .map_err(|_| ERROR_NOACCESS)
    };
    let (text, length) = match decoded {
        Ok(decoded) => decoded,
        Err(1113) => {
            return associated_unavailable(
                env,
                api,
                "icon path contains unpaired UTF-16 surrogates",
                1113,
            );
        }
        Err(error) => return associated_fail(env, error),
    };
    // This direct-source profile preserves the original path (including relative
    // spelling) and WORD ordinal, as native PE/ICO extraction does. Validate both
    // inout spans before creating an icon; no MAX_PATH-sized write is invented.
    if check_range(env.cpu, path, length, Prot::WRITE).is_err()
        || check_range(env.cpu, index.addr(), 2, Prot::RW).is_err()
    {
        return associated_fail(env, ERROR_NOACCESS);
    }
    let ordinal = match env.cpu.read_u16(index.addr()) {
        Ok(ordinal) => usize::from(ordinal),
        Err(_) => return associated_fail(env, ERROR_NOACCESS),
    };
    let saved_error = last_error(env);
    let bytes = match file_bytes(env, &text, api) {
        Ok(bytes) => bytes,
        Err(error) => return associated_fail(env, error),
    };
    let portable_executable = bytes.starts_with(b"MZ");
    let groups = if portable_executable {
        pe_groups(&bytes)
    } else {
        ico_groups(&bytes)
    };
    let Some(groups) = groups else {
        return associated_unavailable(
            env,
            api,
            "direct icon source is unsupported or malformed; shell file associations are unavailable",
            50,
        );
    };
    // A PE WORD selects the zero-based group ordinal, not RT_GROUP_ICON's ID.
    // An ICO has one logical icon with several sizes, independent of its WORD.
    let group = if portable_executable {
        groups.get(ordinal)
    } else {
        groups.first()
    };
    let Some(group) = group else {
        return associated_unavailable(
            env,
            api,
            "requested file icon is absent; shell file associations are unavailable",
            50,
        );
    };
    let Some(image) = image(group, 32) else {
        return associated_unavailable(
            env,
            api,
            "direct icon has no decodable classic DIB pixels; shell file associations are unavailable",
            50,
        );
    };
    let description = format!("associated icon extracted {text} {:?} 32x32", group.id);
    let Some(handle) = env.ext.get_mut::<Win32>().user.objects.create_icon(
        &mut env.sys.objects,
        description,
        image,
    ) else {
        return associated_fail(env, 8);
    };
    // Success LastError is unspecified; never reproduce incidental host loader
    // errors (native PE and ICO extraction leave different internal values).
    set_last_error(env, saved_error);
    Handle(handle)
}

#[win32(shell32, name = "ExtractAssociatedIconA", level = implement)]
fn associated_a(
    env: &mut Env<'_>,
    _instance: Handle,
    path: AnsiStr,
    index: GuestPtr<u16>,
) -> Handle {
    associated(env, path.0.addr(), index, false)
}

#[win32(shell32, name = "ExtractAssociatedIconW", level = implement)]
fn associated_w(
    env: &mut Env<'_>,
    _instance: Handle,
    path: WideStr,
    index: GuestPtr<u16>,
) -> Handle {
    associated(env, path.0.addr(), index, true)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(extract_a_ABI);
    registry.register(extract_w_ABI);
    registry.register(associated_a_ABI);
    registry.register(associated_w_ABI);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user32::image::decode::{Candidate, Group, bitmap};

    fn color_dib(size: u32, color: [u8; 4]) -> Vec<u8> {
        let mask_stride = size.div_ceil(32) * 4;
        let mut bytes = vec![0; (40 + size * size * 4 + mask_stride * size) as usize];
        bytes[..4].copy_from_slice(&40u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        bytes[8..12].copy_from_slice(&(size * 2).to_le_bytes());
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
        for pixel in bytes[40..40 + (size * size * 4) as usize]
            .as_chunks_mut::<4>()
            .0
        {
            pixel.copy_from_slice(&color);
        }
        bytes
    }

    #[test]
    fn dib_pixels_alpha_and_size_selection_are_real_and_bounds_checked() {
        let large = color_dib(32, [0, 255, 0, 123]);
        let small = color_dib(16, [0, 0, 255, 255]);
        let group = Group {
            id: None,
            images: vec![
                Candidate {
                    width: 16,
                    height: 16,
                    bits: 32,
                    bytes: small,
                },
                Candidate {
                    width: 32,
                    height: 32,
                    bits: 32,
                    bytes: large.clone(),
                },
            ],
        };
        let extracted = image(&group, 32).unwrap();
        assert_eq!((extracted.width, extracted.height), (32, 32));
        assert!(
            extracted
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|&pixel| pixel == [0, 255, 0, 123])
        );
        assert!(extracted.mask.iter().all(|&value| value == 0));
        assert_eq!(&image(&group, 16).unwrap().pixels[..4], &[255, 0, 0, 255]);
        assert!(bitmap(&large[..large.len() - 1], 32).is_none());
        assert!(bitmap(b"\x89PNG\r\n\x1a\n", 32).is_none());
        let mut compressed = large;
        compressed[16..20].copy_from_slice(&3u32.to_le_bytes());
        assert!(bitmap(&compressed, 32).is_none());
    }

    #[test]
    fn core_monochrome_icon_retains_its_and_xor_information() {
        let mut bytes = vec![0; 26];
        bytes[..4].copy_from_slice(&12u32.to_le_bytes());
        for (offset, value) in [(4, 1u16), (6, 2), (8, 1), (10, 1)] {
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes[15..18].fill(255); // Palette 1 is white.
        bytes[18] = 0x80; // XOR 1.
        bytes[22] = 0x80; // AND 1, the invert pixel rather than an opaque white.
        let decoded = bitmap(&bytes, 1).unwrap();
        assert_eq!(decoded.pixels, [255, 255, 255, 0]);
        assert_eq!(decoded.mask, [1]);
    }
}
