//! shlwapi (Path*), shell32, imm32, version, and the dialogs and network probes a game may
//! import but that have no counterpart here (comdlg32, SensApi), plus guest-owned
//! COM task memory and Automation strings.

use adv_machine::{Env, Outcome, Registry};

mod ole_memory;

use crate::util::{cstr, put_cstr, set_last_error};
use crate::{Codec, Win32, stdcall};

const SHLWAPI: &str = "shlwapi.dll";
const SHELL32: &str = "shell32.dll";
const IMM32: &str = "imm32.dll";
const COMDLG32: &str = "comdlg32.dll";
const SENSAPI: &str = "sensapi.dll";
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
    // A trailing separator is skipped, like the real function.
    let trimmed = if s.len() > 1 && s.ends_with(b"\\") {
        &s[..s.len() - 1]
    } else {
        &s[..]
    };
    Outcome::Return(p + last_sep(&c, trimmed).map_or(0, |i| i as u32 + 1))
}

fn path_find_extension_a(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let s = cstr(env.cpu, p);
    let c = codec(env);
    Outcome::Return(p + ext_dot(&c, &s).unwrap_or(s.len()) as u32)
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

fn join(codec: &Codec, a: &[u8], b: &[u8]) -> Vec<u8> {
    if b.first() == Some(&b'\\') || b.get(1) == Some(&b':') || a.is_empty() {
        return b.to_vec();
    }
    let mut out = a.to_vec();
    if last_sep(codec, a).is_none_or(|i| i + 1 != a.len()) {
        out.push(b'\\');
    }
    out.extend_from_slice(b);
    out
}

fn path_append_a(env: &mut Env<'_>) -> Outcome {
    let (p, m) = (env.arg(0), env.arg(1));
    let (a, b) = (cstr(env.cpu, p), cstr(env.cpu, m));
    let out = join(&codec(env), &a, &b);
    Outcome::Return(u32::from(put_cstr(env.cpu, p, MAX_PATH, &out).is_some()))
}

fn path_combine_a(env: &mut Env<'_>) -> Outcome {
    let (d, a, b) = (env.arg(0), env.arg(1), env.arg(2));
    let (sa, sb) = (cstr(env.cpu, a), cstr(env.cpu, b));
    let out = join(&codec(env), &sa, &sb);
    let full = env
        .ext
        .get::<Win32>()
        .vfs
        .normalize(&codec(env).decode(&out));
    let out = codec(env).encode(&full);
    match put_cstr(env.cpu, d, MAX_PATH, &out) {
        Some(_) => Outcome::Return(d),
        None => Outcome::Return(0),
    }
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
    let user = format!("C:\\Users\\{}", w.cfg.user_name);
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

fn sh_get_special_folder_path_a(env: &mut Env<'_>) -> Outcome {
    let (buf, csidl) = (env.arg(1), env.arg(2));
    Outcome::Return(u32::from(get_special_folder(env, buf, csidl)))
}

fn sh_get_folder_path_a(env: &mut Env<'_>) -> Outcome {
    let (csidl, buf) = (env.arg(1), env.arg(4));
    Outcome::Return(if get_special_folder(env, buf, csidl) {
        0
    } else {
        0x8000_4005
    })
}

fn shell_execute_a(env: &mut Env<'_>) -> Outcome {
    let c = codec(env);
    let (op, file, params) = (
        cstr(env.cpu, env.arg(1)),
        cstr(env.cpu, env.arg(2)),
        cstr(env.cpu, env.arg(3)),
    );
    log::info!(
        "ShellExecuteA({:?}, {:?}, {:?}): not launched",
        c.decode(&op),
        c.decode(&file),
        c.decode(&params)
    );
    Outcome::Return(42)
}

pub fn register(reg: &mut Registry) {
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
    stdcall(reg, SHLWAPI, "PathIsDirectoryA", 1, path_is_directory_a);
    stdcall(
        reg,
        SHELL32,
        "SHGetSpecialFolderPathA",
        4,
        sh_get_special_folder_path_a,
    );
    stdcall(reg, SHELL32, "SHGetFolderPathA", 5, sh_get_folder_path_a);
    stdcall(reg, SHELL32, "ShellExecuteA", 6, shell_execute_a);
    // File and folder pickers: the user cancels (CommDlgExtendedError stays 0).
    stdcall(reg, COMDLG32, "GetOpenFileNameA", 1, |_| Outcome::Return(0));
    stdcall(reg, COMDLG32, "GetSaveFileNameA", 1, |_| Outcome::Return(0));
    stdcall(reg, COMDLG32, "CommDlgExtendedError", 0, |_| {
        Outcome::Return(0)
    });
    stdcall(reg, SHELL32, "SHBrowseForFolderA", 1, |_| {
        Outcome::Return(0)
    });
    stdcall(reg, SHELL32, "SHGetPathFromIDListA", 2, |_| {
        Outcome::Return(0)
    });
    stdcall(reg, SHELL32, "SHGetFileInfoA", 5, |_| Outcome::Return(0));
    ole_memory::register(reg);
    // Offline, with no error: the guest never reaches the network.
    stdcall(reg, SENSAPI, "IsNetworkAlive", 1, |e| {
        let flags = e.arg(0);
        crate::util::out_u32(e.cpu, flags, 0);
        set_last_error(e, 0);
        Outcome::Return(0)
    });
    stdcall(reg, IMM32, "ImmGetContext", 1, |_| {
        Outcome::Return(0x0001_0001)
    });
    stdcall(reg, IMM32, "ImmReleaseContext", 2, |_| Outcome::Return(1));
    stdcall(reg, IMM32, "ImmSetOpenStatus", 2, |_| Outcome::Return(1));
    stdcall(reg, IMM32, "ImmGetOpenStatus", 1, |_| Outcome::Return(0));
    stdcall(reg, IMM32, "ImmSetConversionStatus", 3, |_| {
        Outcome::Return(1)
    });
    stdcall(reg, IMM32, "ImmAssociateContext", 2, |_| Outcome::Return(0));
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
    }
}
