//! shlwapi (Path*), shell32, imm32, version, and the dialogs and network probes a game may
//! import but that have no counterpart here (comdlg32, SensApi), plus guest-owned
//! COM task memory and Automation strings.

use adv_abi::Registry;
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};

mod common_dialogs;
mod dwm;
mod folder_browse;
mod gdiplus;
mod hglobal_stream;
mod image_list;
pub(crate) mod imm;
mod internet;
mod ole_memory;
mod path_match;
mod property_sheet;
mod shell_drop;
mod shell_execute;
mod shell_icon;
mod shell_path;
mod task_allocator;
#[cfg(test)]
mod test_support;
mod theme;
mod variant;
mod wts;

use crate::util::{cstr, put_cstr, put_wstr, set_last_error};
use adv_os::Codec;

use crate::{Win32, stdcall};

const SHLWAPI: &str = "shlwapi.dll";
const SHELL32: &str = "shell32.dll";
const SHFOLDER: &str = "shfolder.dll";
const SENSAPI: &str = "sensapi.dll";
const COMCTL32: &str = "comctl32.dll";
const MAX_PATH: u32 = 260;

/// Byte offsets of each character start (DBCS-aware: a trail byte may be 0x5C `\`).
fn char_starts(codec: &Codec, s: &[u8]) -> Vec<usize> {
    let mut v = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        v.push(i);
        i += if codec.is_lead_byte(0, s[i]) && i + 1 < s.len() {
            2
        } else {
            1
        };
    }
    v
}

/// Offset of the last path separator (`\` or `/`) that is a whole character.
fn last_sep(codec: &Codec, s: &[u8]) -> Option<usize> {
    char_starts(codec, s)
        .into_iter()
        .rev()
        .find(|&i| s[i] == b'\\' || s[i] == b'/')
}

/// Offset of the file name's extension dot, if any.
fn ext_dot(codec: &Codec, s: &[u8]) -> Option<usize> {
    let name_start = last_sep(codec, s).map_or(0, |i| i + 1);
    char_starts(codec, s)
        .into_iter()
        .rev()
        .take_while(|&i| i >= name_start)
        .find(|&i| s[i] == b'.')
}

fn codec(env: &Env<'_>) -> Codec {
    env.ext.get::<Win32>().codec
}

fn path_remove_file_spec_a(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let s = cstr(env.cpu, p);
    let c = codec(env);
    let cut = match last_sep(&c, &s) {
        // Keep the separator of a root ("C:\" or "\").
        Some(i) if i == 0 || (i == 2 && s.get(1) == Some(&b':')) => i + 1,
        Some(i) => i,
        None if s.len() >= 2 && s[1] == b':' => 2,
        None => 0,
    };
    if cut >= s.len() {
        return Outcome::Return(0);
    }
    let _ = env.cpu.write(p + cut as u32, &[0]);
    Outcome::Return(1)
}

fn path_find_file_name_a(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let s = cstr(env.cpu, p);
    let c = codec(env);
    let offset = filename_offset(
        char_starts(&c, &s)
            .into_iter()
            .map(|i| (i, u32::from(s[i]))),
    );
    Outcome::Return(p + offset as u32)
}

fn filename_offset(units: impl Iterator<Item = (usize, u32)>) -> usize {
    let mut units = units.peekable();
    let mut start = 0;
    while let Some((_, unit)) = units.next() {
        if matches!(unit, 47 | 58 | 92)
            && let Some(&(index, next)) = units.peek()
            && !matches!(next, 47 | 92)
        {
            start = index;
        }
    }
    start
}

#[adv_abi::win32(shlwapi, name = "PathFindFileNameW", level = implement)]
fn path_find_file_name_w(env: &mut Env<'_>, path: adv_abi::WideStr) -> Outcome {
    let Ok(units) = path.read(env.cpu, 65536) else {
        set_last_error(env, 998);
        return Outcome::Return(0);
    };
    let offset = filename_offset(
        units
            .iter()
            .enumerate()
            .map(|(i, unit)| (i, u32::from(*unit))),
    );
    Outcome::Return(path.0.addr() + offset as u32 * 2)
}

fn path_find_extension_a(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let s = cstr(env.cpu, p);
    let c = codec(env);
    let offset = extension_offset(
        char_starts(&c, &s)
            .into_iter()
            .map(|i| (i, u32::from(s[i]))),
        s.len(),
    );
    Outcome::Return(p + offset as u32)
}

fn extension_offset(units: impl Iterator<Item = (usize, u32)>, end: usize) -> usize {
    let mut dot = None;
    for (index, unit) in units {
        match unit {
            46 => dot = Some(index),
            32 | 92 => dot = None,
            _ => {}
        }
    }
    dot.unwrap_or(end)
}

#[adv_abi::win32(shlwapi, name = "PathFindExtensionW", level = implement)]
fn path_find_extension_w(env: &mut Env<'_>, path: adv_abi::WideStr) -> Outcome {
    let Ok(units) = path.read(env.cpu, 65536) else {
        set_last_error(env, 998);
        return Outcome::Return(0);
    };
    let offset = extension_offset(
        units
            .iter()
            .enumerate()
            .map(|(i, unit)| (i, u32::from(*unit))),
        units.len(),
    );
    Outcome::Return(path.0.addr() + offset as u32 * 2)
}

fn path_remove_extension_a(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let s = cstr(env.cpu, p);
    let c = codec(env);
    if let Some(d) = ext_dot(&c, &s) {
        let _ = env.cpu.write(p + d as u32, &[0]);
    }
    Outcome::Return(0)
}

fn path_rename_extension_a(env: &mut Env<'_>) -> Outcome {
    let (p, e) = (env.arg(0), env.arg(1));
    let s = cstr(env.cpu, p);
    let ext = cstr(env.cpu, e);
    let c = codec(env);
    let base = ext_dot(&c, &s).unwrap_or(s.len());
    let mut out = s[..base].to_vec();
    out.extend_from_slice(&ext);
    Outcome::Return(u32::from(put_cstr(env.cpu, p, MAX_PATH, &out).is_some()))
}

fn path_add_extension_a(env: &mut Env<'_>) -> Outcome {
    let (p, e) = (env.arg(0), env.arg(1));
    let s = cstr(env.cpu, p);
    let c = codec(env);
    if ext_dot(&c, &s).is_some() {
        return Outcome::Return(0);
    }
    let ext = if e == 0 {
        b".exe".to_vec()
    } else {
        cstr(env.cpu, e)
    };
    let out = [&s[..], &ext[..]].concat();
    Outcome::Return(u32::from(put_cstr(env.cpu, p, MAX_PATH, &out).is_some()))
}

fn path_add_backslash_a(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let s = cstr(env.cpu, p);
    let c = codec(env);
    let ends_with_sep = last_sep(&c, &s).is_some_and(|i| i + 1 == s.len());
    if ends_with_sep || s.is_empty() {
        return Outcome::Return(p + s.len() as u32);
    }
    let out = [&s[..], b"\\"].concat();
    match put_cstr(env.cpu, p, MAX_PATH, &out) {
        Some(n) => Outcome::Return(p + n),
        None => Outcome::Return(0),
    }
}

fn path_append_a(env: &mut Env<'_>) -> Outcome {
    shell_path::join(env, false, true)
}
fn path_combine_a(env: &mut Env<'_>) -> Outcome {
    shell_path::join(env, false, false)
}

fn path_unquote_spaces_a(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let s = cstr(env.cpu, p);
    if s.len() >= 2 && s[0] == b'"' && s[s.len() - 1] == b'"' {
        put_cstr(env.cpu, p, MAX_PATH, &s[1..s.len() - 1]);
    }
    Outcome::Return(0)
}

fn path_strip_path_a(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let s = cstr(env.cpu, p);
    if let Some(i) = last_sep(&codec(env), &s) {
        put_cstr(env.cpu, p, MAX_PATH, &s[i + 1..]);
    }
    Outcome::Return(0)
}

fn path_file_exists_a(env: &mut Env<'_>) -> Outcome {
    let s = codec(env).decode(&cstr(env.cpu, env.arg(0)));
    let found = env.ext.get::<Win32>().vfs.lookup(&s).is_some();
    if !found {
        set_last_error(env, crate::util::err::FILE_NOT_FOUND);
    }
    Outcome::Return(u32::from(found))
}

fn path_file_exists_w(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    if pointer == 0 {
        return Outcome::Return(0);
    }
    if matches!(env.cpu.read_u16(pointer), Ok(0)) {
        set_last_error(env, crate::util::err::PATH_NOT_FOUND);
        return Outcome::Return(0);
    }
    // Use the normal attribute entry so overlays, system files and guest NT
    // attribute providers have exactly the same view as GetFileAttributesW.
    match env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "GetFileAttributesW")
    {
        Ok(entry) => Outcome::call(entry, [pointer], |_, resumed| {
            Outcome::Return(u32::from(resumed.value() != u32::MAX))
        }),
        Err(error) => Outcome::Fail(error.to_string()),
    }
}

fn path_is_directory_a(env: &mut Env<'_>) -> Outcome {
    let s = codec(env).decode(&cstr(env.cpu, env.arg(0)));
    Outcome::Return(if env.ext.get::<Win32>().vfs.is_dir(&s) {
        0x10
    } else {
        0
    })
}

/// Guest path of a CSIDL folder, or `None` if unsupported.
fn special_folder(w: &Win32, csidl: u32) -> Option<String> {
    let user = format!("C:\\Users\\{}", w.cfg.os.user_name);
    Some(match csidl & 0xff {
        0x00 | 0x10 => format!("{user}\\Desktop"),
        0x05 => format!("{user}\\Documents"),
        0x1a => format!("{user}\\AppData\\Roaming"),
        0x1c => format!("{user}\\AppData\\Local"),
        0x23 => "C:\\ProgramData".into(),
        0x24 => "C:\\Windows".into(),
        0x25 => "C:\\Windows\\system32".into(),
        0x26 => "C:\\Program Files".into(),
        0x28 => user,
        0x2e => "C:\\Users\\Public\\Documents".into(),
        _ => return None,
    })
}

fn get_special_folder(env: &mut Env<'_>, buf: u32, csidl: u32) -> bool {
    let w = env.ext.get::<Win32>();
    let Some(path) = special_folder(w, csidl) else {
        log::warn!("special folder CSIDL {csidl:#x} not supported");
        return false;
    };
    // Folders always exist (CSIDL_FLAG_CREATE or not): create the host directory.
    if let Some(host) = w.vfs.write_path(&path) {
        let _ = std::fs::create_dir_all(host);
    }
    let b = w.codec.encode(&path);
    put_cstr(env.cpu, buf, MAX_PATH, &b).is_some()
}

fn get_special_folder_w(env: &mut Env<'_>, buf: u32, csidl: u32) -> bool {
    let w = env.ext.get::<Win32>();
    let Some(path) = special_folder(w, csidl) else {
        log::warn!("special folder CSIDL {csidl:#x} not supported");
        return false;
    };
    if let Some(host) = w.vfs.write_path(&path) {
        let _ = std::fs::create_dir_all(host);
    }
    let u: Vec<u16> = path.encode_utf16().collect();
    put_wstr(env.cpu, buf, MAX_PATH, &u).is_some()
}

fn sh_get_special_folder_path_a(env: &mut Env<'_>) -> Outcome {
    let (buf, csidl) = (env.arg(1), env.arg(2));
    Outcome::Return(u32::from(get_special_folder(env, buf, csidl)))
}

fn sh_get_special_folder_path_w(env: &mut Env<'_>) -> Outcome {
    let (buf, csidl) = (env.arg(1), env.arg(2));
    Outcome::Return(u32::from(get_special_folder_w(env, buf, csidl)))
}

fn sh_get_folder_path_a(env: &mut Env<'_>) -> Outcome {
    let (csidl, buf) = (env.arg(1), env.arg(4));
    Outcome::Return(if get_special_folder(env, buf, csidl) {
        0
    } else {
        0x8000_4005
    })
}

fn sh_get_folder_path_w(env: &mut Env<'_>) -> Outcome {
    let (csidl, buf) = (env.arg(1), env.arg(4));
    Outcome::Return(if get_special_folder_w(env, buf, csidl) {
        0
    } else {
        0x8000_4005
    })
}

pub fn register(reg: &mut Registry) {
    dwm::register(reg);
    wts::register(reg);
    reg.register(path_find_extension_w_ABI);
    reg.register(path_find_file_name_w_ABI);
    path_match::register(reg);
    theme::register(reg);
    internet::register(reg);
    shell_execute::register(reg);
    shell_icon::register(reg);
    shell_path::register(reg);
    shell_drop::register(reg);
    property_sheet::register(reg);
    stdcall(
        reg,
        SHLWAPI,
        "PathRemoveFileSpecA",
        1,
        path_remove_file_spec_a,
    );
    stdcall(reg, SHLWAPI, "PathFindFileNameA", 1, path_find_file_name_a);
    stdcall(reg, SHLWAPI, "PathFindExtensionA", 1, path_find_extension_a);
    stdcall(
        reg,
        SHLWAPI,
        "PathRemoveExtensionA",
        1,
        path_remove_extension_a,
    );
    stdcall(
        reg,
        SHLWAPI,
        "PathRenameExtensionA",
        2,
        path_rename_extension_a,
    );
    stdcall(reg, SHLWAPI, "PathAddExtensionA", 2, path_add_extension_a);
    stdcall(reg, SHLWAPI, "PathAddBackslashA", 1, path_add_backslash_a);
    stdcall(reg, SHLWAPI, "PathAppendA", 2, path_append_a);
    stdcall(reg, SHLWAPI, "PathCombineA", 3, path_combine_a);
    stdcall(reg, SHLWAPI, "PathUnquoteSpacesA", 1, path_unquote_spaces_a);
    stdcall(reg, SHLWAPI, "PathStripPathA", 1, path_strip_path_a);
    stdcall(reg, SHLWAPI, "PathFileExistsA", 1, path_file_exists_a);
    stdcall(reg, SHLWAPI, "PathFileExistsW", 1, path_file_exists_w);
    stdcall(reg, SHLWAPI, "PathIsDirectoryA", 1, path_is_directory_a);
    stdcall(
        reg,
        SHELL32,
        "SHGetSpecialFolderPathA",
        4,
        sh_get_special_folder_path_a,
    );
    stdcall(
        reg,
        SHELL32,
        "SHGetSpecialFolderPathW",
        4,
        sh_get_special_folder_path_w,
    );
    stdcall(reg, SHELL32, "SHGetFolderPathA", 5, sh_get_folder_path_a);
    stdcall(reg, SHELL32, "SHGetFolderPathW", 5, sh_get_folder_path_w);
    stdcall(reg, SHFOLDER, "SHGetFolderPathA", 5, sh_get_folder_path_a);
    stdcall(reg, SHFOLDER, "SHGetFolderPathW", 5, sh_get_folder_path_w);
    common_dialogs::register(reg);
    folder_browse::register(reg);
    gdiplus::register(reg);
    hglobal_stream::register(reg);
    ole_memory::register(reg);
    task_allocator::register(reg);
    variant::register(reg);
    // Offline, with no error: the guest never reaches the network.
    stdcall(reg, SENSAPI, "IsNetworkAlive", 1, |e| {
        let flags = e.arg(0);
        crate::util::out_u32(e.cpu, flags, 0);
        set_last_error(e, 0);
        Outcome::Return(0)
    });
    imm::register(reg);
    stdcall(reg, "mpr.dll", "WNetGetUniversalNameA", 4, |_| {
        Outcome::Return(2250) // ERROR_NOT_CONNECTED
    });
    stdcall(reg, "mpr.dll", "WNetGetUniversalNameW", 4, |_| {
        Outcome::Return(2250) // ERROR_NOT_CONNECTED
    });
    stdcall(reg, COMCTL32, "InitCommonControls", 0, |_| {
        Outcome::Return(0)
    });
    stdcall(reg, COMCTL32, "InitCommonControlsEx", 1, |_| {
        Outcome::Return(1)
    });
    image_list::register(reg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dbcs_trail_backslash_is_not_a_separator() {
        let c = Codec::new(932);
        // "C:\表" where 表 = 95 5C: the only separator is at offset 2.
        let s = [b'C', b':', b'\\', 0x95, 0x5c];
        assert_eq!(last_sep(&c, &s), Some(2));
        let s = [b'a', 0x95, 0x5c, b'.', b't', b'x', b't'];
        assert_eq!(ext_dot(&c, &s), Some(3));
        let s = [b'a', b'.', b'b', 0x95, 0x5c];
        assert_eq!(
            extension_offset(
                char_starts(&c, &s)
                    .into_iter()
                    .map(|i| (i, u32::from(s[i]))),
                s.len()
            ),
            1
        );
        for (path, expected) in [
            ("abc", 3),
            ("a.b c", 5),
            ("a.b/c", 1),
            ("a.b:c", 1),
            (r"a.b\c", 5),
            (".a", 0),
            ("a..b", 2),
            ("字體.dll", 2),
        ] {
            let units: Vec<_> = path.encode_utf16().collect();
            assert_eq!(
                extension_offset(
                    units.iter().enumerate().map(|(i, u)| (i, u32::from(*u))),
                    units.len()
                ),
                expected
            );
        }
    }

    #[test]
    fn common_controls_registered() {
        let mut reg = Registry::default();
        register(&mut reg);
        assert!(reg.get("comctl32.dll", "InitCommonControls").is_some());
        assert!(reg.get("comctl32.dll", "InitCommonControlsEx").is_some());
        assert!(reg.get("comctl32.dll", "ImageList_Read").is_some());
    }

    #[test]
    fn filename_offsets_match_native_roots_colons_and_trailing_separators() {
        for (path, expected) in [
            ("abc", 0),
            ("C:abc", 2),
            (r"C:\", 0),
            (r"abc\", 0),
            ("abc/", 0),
            (r"a\\b", 3),
            ("a//b", 3),
            ("C:/b", 3),
            (r"a\b\\", 2),
            (r"\\server\share\", 9),
        ] {
            assert_eq!(
                filename_offset(
                    path.encode_utf16()
                        .enumerate()
                        .map(|(i, u)| (i, u32::from(u)))
                ),
                expected
            );
        }
        let codec = Codec::new(932);
        let path = [b'a', b'\\', 0x95, 0x5c, b'b'];
        assert_eq!(
            filename_offset(
                char_starts(&codec, &path)
                    .into_iter()
                    .map(|i| (i, u32::from(path[i])))
            ),
            2
        );
    }
}
