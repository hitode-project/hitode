//! Shell path syntax stays lexical; it does not use the process VFS cwd.
use crate::{Win32, util::set_last_error};
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, win32};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};

const SLASH: u32 = 92;
const COLON: u32 = 58;
const DOT: u32 = 46;
const MAX_PATH: usize = 260;

fn input(env: &Env<'_>, pointer: u32, wide: bool) -> Option<Vec<u32>> {
    if pointer == 0 {
        return Some(Vec::new());
    }
    if wide {
        WideStr(GuestPtr::new(pointer))
            .read(env.cpu, 65536)
            .ok()
            .map(|s| s.into_iter().map(u32::from).collect())
    } else {
        let bytes = AnsiStr(GuestPtr::new(pointer)).read(env.cpu, 65536).ok()?;
        let codec = env.ext.get::<Win32>().codec;
        let mut result = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let count = if codec.is_lead_byte(0, bytes[at]) && at + 1 < bytes.len() {
                2
            } else {
                1
            };
            result.push(if count == 2 {
                u32::from(u16::from_be_bytes([bytes[at], bytes[at + 1]]))
            } else {
                u32::from(bytes[at])
            });
            at += count;
        }
        Some(result)
    }
}

fn output(env: &mut Env<'_>, pointer: u32, units: &[u32], wide: bool) -> Option<u32> {
    let mut bytes = Vec::new();
    for &unit in units {
        if wide {
            bytes.extend_from_slice(&(unit as u16).to_le_bytes());
        } else if unit > 255 {
            bytes.extend_from_slice(&(unit as u16).to_be_bytes());
        } else {
            bytes.push(unit as u8);
        }
    }
    let count = bytes.len() / if wide { 2 } else { 1 };
    if count >= MAX_PATH {
        return None;
    }
    bytes.extend_from_slice(if wide { &[0, 0] } else { &[0] });
    if adv_abi::guest::check_range(env.cpu, pointer, bytes.len() as u32, Prot::WRITE).is_err()
        || env.cpu.write(pointer, &bytes).is_err()
    {
        return None;
    }
    Some(count as u32)
}

fn root(path: &[u32]) -> Option<Vec<u32>> {
    if path.get(1) == Some(&COLON) {
        return Some(vec![path[0], COLON, SLASH]);
    }
    if path.first() != Some(&SLASH) {
        return None;
    }
    if path.get(1) != Some(&SLASH) {
        return Some(vec![SLASH]);
    }
    let mut end = path.len();
    let mut separators = 0;
    for (at, &unit) in path.iter().enumerate().skip(2) {
        if unit == SLASH {
            separators += 1;
            if separators == 2 {
                end = at;
                break;
            }
        }
    }
    let mut result = path[..end].to_vec();
    if result.last() != Some(&SLASH) {
        result.push(SLASH);
    }
    Some(result)
}

fn canonical(path: &[u32]) -> Vec<u32> {
    let (mut prefix, rest, separate) = if path.starts_with(&[SLASH, SLASH]) {
        let end = path[2..]
            .iter()
            .position(|&u| u == SLASH)
            .map_or(path.len(), |i| i + 2);
        (path[..end].to_vec(), &path[end..], true)
    } else if path.get(1) == Some(&COLON) && path.get(2) == Some(&SLASH) {
        (path[..3].to_vec(), &path[3..], false)
    } else if path.first() == Some(&SLASH) {
        (vec![SLASH], &path[1..], false)
    } else {
        (Vec::new(), path, false)
    };
    let rest = if separate {
        rest.strip_prefix(&[SLASH]).unwrap_or(rest)
    } else {
        rest
    };
    let mut parts: Vec<&[u32]> = Vec::new();
    for part in rest.split(|&u| u == SLASH) {
        if part == [DOT] {
            continue;
        }
        if part == [DOT, DOT] {
            let removed = parts.pop().is_some();
            if removed && parts.is_empty() && (prefix.is_empty() || prefix.last() == Some(&COLON)) {
                prefix.push(SLASH);
            }
        } else {
            parts.push(part);
        }
    }
    let mut result = prefix;
    for (index, part) in parts.iter().enumerate() {
        if index > 0 || (separate && !result.is_empty() && result.last() != Some(&SLASH)) {
            result.push(SLASH);
        }
        result.extend_from_slice(part);
    }
    if result.is_empty() || (result.len() == 2 && result[1] == COLON) {
        result.push(SLASH);
    }
    result
}

fn combine(dir: &[u32], file: &[u32]) -> Option<Vec<u32>> {
    let path = if dir.is_empty() || file.starts_with(&[SLASH, SLASH]) || file.get(1) == Some(&COLON)
    {
        file.to_vec()
    } else if file.is_empty() {
        dir.to_vec()
    } else if file.first() == Some(&SLASH) {
        let mut result = root(dir)?;
        result.extend_from_slice(&file[1..]);
        result
    } else {
        let mut result = dir.to_vec();
        if result.last() != Some(&SLASH) {
            result.push(SLASH);
        }
        result.extend_from_slice(file);
        result
    };
    Some(canonical(&path))
}

pub(super) fn join(env: &mut Env<'_>, wide: bool, append: bool) -> Outcome {
    let (destination, dir_pointer, file_pointer) = if append {
        (env.arg(0), env.arg(0), env.arg(1))
    } else {
        (env.arg(0), env.arg(1), env.arg(2))
    };
    if destination == 0 || (!append && dir_pointer == 0 && file_pointer == 0) {
        if destination != 0 {
            let _ = output(env, destination, &[], wide);
        }
        return Outcome::Return(0);
    }
    let (Some(dir), Some(file)) = (
        input(env, dir_pointer, wide),
        input(env, file_pointer, wide),
    ) else {
        set_last_error(env, 998);
        return Outcome::Return(0);
    };
    let file = if append && !file.starts_with(&[SLASH, SLASH]) {
        &file[file.iter().take_while(|&&u| u == SLASH).count()..]
    } else {
        &file
    };
    let result = combine(&dir, file).and_then(|units| output(env, destination, &units, wide));
    if result.is_none() && !append {
        let _ = output(env, destination, &[], wide);
    }
    Outcome::Return(if result.is_some() {
        if append { 1 } else { destination }
    } else {
        0
    })
}

#[win32(shlwapi, name = "PathCombineW", level = implement)]
fn combine_w(env: &mut Env<'_>, _out: WideStr, _dir: WideStr, _file: WideStr) -> Outcome {
    join(env, true, false)
}
#[win32(shlwapi, name = "PathAppendW", level = implement)]
fn append_w(env: &mut Env<'_>, _path: WideStr, _more: WideStr) -> Outcome {
    join(env, true, true)
}

#[derive(Clone, Copy)]
enum Edit {
    RemoveFile,
    RemoveExtension,
    RenameExtension,
    AddExtension,
    AddBackslash,
    StripPath,
    Unquote,
}

fn edit(env: &mut Env<'_>, path: WideStr, operation: Edit, extension: Option<WideStr>) -> Outcome {
    let Some(mut units) = input(env, path.0.addr(), true) else {
        set_last_error(env, 998);
        return Outcome::Return(0);
    };
    let original = units.len();
    let ext = super::extension_offset(units.iter().enumerate().map(|(i, &u)| (i, u)), original);
    match operation {
        Edit::RemoveFile => {
            // RemoveFileSpec uses backslashes and drive colons, not forward slashes.
            let cut = match units.iter().rposition(|&u| u == SLASH) {
                Some(i) if i == 0 || (i == 2 && units.get(1) == Some(&COLON)) => i + 1,
                Some(i) => i,
                None if units.get(1) == Some(&COLON) => 2,
                None => 0,
            };
            if cut >= original {
                return Outcome::Return(0);
            }
            units.truncate(cut);
        }
        Edit::RemoveExtension => units.truncate(ext), // RemoveExtension
        Edit::RenameExtension | Edit::AddExtension => {
            if matches!(operation, Edit::AddExtension) && ext != original {
                return Outcome::Return(0);
            }
            let extension = extension.unwrap();
            if extension.0.is_null() && matches!(operation, Edit::RenameExtension) {
                return Outcome::Return(0);
            }
            let added = if extension.0.is_null() {
                vec![DOT, 101, 120, 101]
            } else {
                match input(env, extension.0.addr(), true) {
                    Some(s) => s,
                    None => return Outcome::Return(0),
                }
            };
            if matches!(operation, Edit::RenameExtension) {
                units.truncate(ext);
            }
            units.extend(added);
        }
        Edit::AddBackslash => {
            if !units.is_empty() && units.last() != Some(&SLASH) {
                units.push(SLASH);
            }
        }
        Edit::StripPath => {
            let at = super::filename_offset(units.iter().enumerate().map(|(i, &u)| (i, u)));
            units.drain(..at);
        }
        Edit::Unquote => {
            if units.len() >= 2 && units.first() == Some(&34) && units.last() == Some(&34) {
                units.pop();
                units.remove(0);
            }
        }
    }
    let Some(count) = output(env, path.0.addr(), &units, true) else {
        return Outcome::Return(0);
    };
    Outcome::Return(match operation {
        Edit::RemoveFile | Edit::RenameExtension | Edit::AddExtension => 1,
        Edit::AddBackslash => path.0.addr() + count * 2,
        _ => 0,
    })
}
#[win32(shlwapi, name = "PathRemoveFileSpecW", level = implement)]
fn remove_file(env: &mut Env<'_>, path: WideStr) -> Outcome {
    edit(env, path, Edit::RemoveFile, None)
}
#[win32(shlwapi, name = "PathRemoveExtensionW", level = implement)]
fn remove_extension(env: &mut Env<'_>, path: WideStr) -> Outcome {
    edit(env, path, Edit::RemoveExtension, None)
}
#[win32(shlwapi, name = "PathRenameExtensionW", level = implement)]
fn rename_extension(env: &mut Env<'_>, path: WideStr, ext: WideStr) -> Outcome {
    edit(env, path, Edit::RenameExtension, Some(ext))
}
#[win32(shlwapi, name = "PathAddExtensionW", level = implement)]
fn add_extension(env: &mut Env<'_>, path: WideStr, ext: WideStr) -> Outcome {
    edit(env, path, Edit::AddExtension, Some(ext))
}
#[win32(shlwapi, name = "PathAddBackslashW", level = implement)]
fn add_backslash(env: &mut Env<'_>, path: WideStr) -> Outcome {
    edit(env, path, Edit::AddBackslash, None)
}
#[win32(shlwapi, name = "PathStripPathW", level = implement)]
fn strip_path(env: &mut Env<'_>, path: WideStr) -> Outcome {
    edit(env, path, Edit::StripPath, None)
}
#[win32(shlwapi, name = "PathUnquoteSpacesW", level = implement)]
fn unquote(env: &mut Env<'_>, path: WideStr) -> Outcome {
    edit(env, path, Edit::Unquote, None)
}
#[win32(shlwapi, name = "PathIsDirectoryW", level = implement)]
fn directory(env: &mut Env<'_>, path: WideStr) -> Outcome {
    if path.0.is_null() {
        return Outcome::Return(0);
    }
    let entry = match env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "GetFileAttributesW")
    {
        Ok(entry) => entry,
        Err(error) => return Outcome::Fail(error.to_string()),
    };
    Outcome::call(entry, [path.0.addr()], |_, resumed| {
        let attributes = resumed.value();
        Outcome::Return(if attributes == u32::MAX {
            0
        } else {
            attributes & 16
        })
    })
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        combine_w_ABI,
        append_w_ABI,
        remove_file_ABI,
        remove_extension_ABI,
        rename_extension_ABI,
        add_extension_ABI,
        add_backslash_ABI,
        strip_path_ABI,
        unquote_ABI,
        directory_ABI,
    ] {
        registry.register(api);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u32> {
        text.encode_utf16().map(u32::from).collect()
    }

    #[test]
    fn lexical_combinations_preserve_roots_relative_names_and_shell_dot_rules() {
        for (dir, file, expected) in [
            ("a", "b", Some(r"a\b")),
            (r"C:\dir", r"\file", Some(r"C:\file")),
            (r"C:\dir", r"..\file", Some(r"C:\file")),
            ("a", r"..\file", Some(r"\file")),
            ("a", r"\file", None),
            ("", r"\file", Some(r"\file")),
            (r"\\srv\share\dir", r"\file", Some(r"\\srv\share\file")),
            (r"\\srv\share\dir", r"..\..\file", Some(r"\\srv\file")),
            ("a/b", "../c", Some(r"a/b\../c")),
            (r"a\b", r"..\c", Some(r"a\c")),
            ("a", r"..\..\b", Some(r"\b")),
            ("a", ".", Some("a")),
            ("", "", Some(r"\")),
            ("a", r"b\\c", Some(r"a\b\\c")),
            ("a", r"b\..\", Some(r"a\")),
            ("a", r"b\\..\c", Some(r"a\b\c")),
            (r"C:dir", r"\file", Some(r"C:\file")),
            (r"C:\dir", "D:foo", Some("D:foo")),
        ] {
            let actual = combine(&units(dir), &units(file));
            assert_eq!(actual, expected.map(units), "{dir:?} + {file:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn lexical_combinations_match_the_native_wide_shell_front() {
        #[link(name = "shlwapi")]
        unsafe extern "system" {
            fn PathCombineW(output: *mut u16, dir: *const u16, file: *const u16) -> *mut u16;
        }
        let dirs = [
            "",
            "a",
            r"a\b",
            r"a\",
            r"C:dir",
            r"C:\dir",
            r"\\srv\share\dir",
            "a/b",
        ];
        let files = [
            "",
            ".",
            "..",
            "b",
            r"..\b",
            r"..\..\b",
            r"\file",
            r"\\srv\share",
            r"b\\c",
            r"b\..\",
            r"b\\..\c",
            "../c",
            "D:foo",
        ];
        for dir in dirs {
            for file in files {
                let (a, b) = (
                    dir.encode_utf16().chain([0]).collect::<Vec<_>>(),
                    file.encode_utf16().chain([0]).collect::<Vec<_>>(),
                );
                let mut out = [0u16; 260];
                let success =
                    unsafe { !PathCombineW(out.as_mut_ptr(), a.as_ptr(), b.as_ptr()).is_null() };
                let end = out.iter().position(|&u| u == 0).unwrap();
                let expected = success.then(|| {
                    out[..end]
                        .iter()
                        .copied()
                        .map(u32::from)
                        .collect::<Vec<_>>()
                });
                assert_eq!(
                    combine(&units(dir), &units(file)),
                    expected,
                    "{dir:?} + {file:?}"
                );
            }
        }
    }
}
