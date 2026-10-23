//! NT file names to host paths, after Wine's `nt_to_unix_file_name`.
//!
//! A name of `\??\`, a drive letter and a colon, then the path on that
//! drive, names a file under `<prefix>/dosdevices/<letter>:`. Each
//! component is looked up as written first and then without regard to case,
//! as on Windows. NT names reject `.` and `..` components.
//! `\??\unix` names the host's root the same way, as in Wine.

use std::path::{Path, PathBuf};

use crate::status::{self, Status};

/// Where a name leads.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// An existing file or directory.
    Found(PathBuf),
    /// The last component does not exist; its directory does.
    Missing(PathBuf),
    /// Not a drive path: a device or another object for the server.
    Device,
}

const INVALID: &[char] = &['*', '?', '<', '>', '|', '"'];
const UNIX: &str = "unix";

fn is_drive(name: &str) -> bool {
    name.len() == 2 && name.ends_with(':') && name.as_bytes()[0].is_ascii_alphabetic()
}

/// The NT name of host file `host`, after Wine's `unix_to_nt_file_name`:
/// on the drive whose root holds it most closely, else under `\??\unix`.
pub fn nt_name(dosdevices: &Path, host: &Path) -> String {
    // A missing file is named after its directory.
    let host = host.canonicalize().unwrap_or_else(|_| {
        match (
            host.parent().and_then(|parent| parent.canonicalize().ok()),
            host.file_name(),
        ) {
            (Some(parent), Some(name)) => parent.join(name),
            _ => host.to_owned(),
        }
    });
    let drives = std::fs::read_dir(dosdevices)
        .into_iter()
        .flatten()
        .flatten();
    let on_drive = drives
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let root = entry.path().canonicalize().ok()?;
            let rest = host.strip_prefix(&root).ok()?.to_owned();
            is_drive(&name).then(|| (root.components().count(), name, rest))
        })
        .max_by_key(|&(depth, ..)| depth);
    let (prefix, rest) = match on_drive {
        Some((_, drive, rest)) => (format!(r"\??\{}\", drive.to_ascii_uppercase()), rest),
        None => (
            format!(r"\??\{UNIX}\"),
            host.strip_prefix("/").unwrap_or(&host).to_owned(),
        ),
    };
    prefix + &rest.to_string_lossy().replace('/', "\\")
}

pub fn lookup(dosdevices: &Path, name: &[u16]) -> Result<Lookup, Status> {
    let name = String::from_utf16(name).map_err(|_| status::OBJECT_NAME_INVALID)?;
    let Some(rest) = name.strip_prefix(r"\??\") else {
        return Ok(Lookup::Device);
    };
    let (drive, mut rest) = rest.split_once('\\').unwrap_or((rest, ""));
    let mut path = if drive == UNIX {
        PathBuf::from("/")
    } else if is_drive(drive) {
        let root = dosdevices.join(drive.to_ascii_lowercase());
        if !root.is_dir() {
            return Err(status::OBJECT_PATH_NOT_FOUND);
        }
        root
    } else {
        if drive.contains('/') {
            return Err(if rest.is_empty() {
                status::OBJECT_NAME_NOT_FOUND
            } else {
                status::OBJECT_PATH_NOT_FOUND
            });
        }
        return Ok(Lookup::Device);
    };
    // Wine accepts one extra backslash immediately after a DOS device.
    if let Some(tail) = rest.strip_prefix('\\') {
        rest = tail;
    }
    validate_components(rest, drive == UNIX)?;
    let trailing = rest.ends_with('\\');
    let components: Vec<&str> = rest
        .trim_end_matches('\\')
        .split('\\')
        .filter(|part| !part.is_empty())
        .collect();

    let last = components.len();
    for (index, component) in components.into_iter().enumerate() {
        if !path.is_dir() {
            return Err(status::OBJECT_PATH_NOT_FOUND);
        }
        match find(&path, component) {
            Some(found) => path = found,
            None if index + 1 == last => return Ok(Lookup::Missing(path.join(component))),
            None => return Err(status::OBJECT_PATH_NOT_FOUND),
        }
    }
    if trailing && !path.is_dir() {
        return Err(status::OBJECT_NAME_INVALID);
    }
    Ok(Lookup::Found(path))
}

pub(crate) fn validate_components(name: &str, unix: bool) -> Result<(), Status> {
    for component in name.strip_suffix('\\').unwrap_or(name).split('\\') {
        if component.is_empty() && !name.is_empty()
            || matches!(component, "." | "..")
            || component.contains('\0')
            || !unix
                && (component.contains(INVALID)
                    || component.contains('/')
                    || component.chars().any(|c| c < ' '))
        {
            return Err(status::OBJECT_NAME_INVALID);
        }
    }
    Ok(())
}

/// The entry of `directory` named `name`, in any case.
fn find(directory: &Path, name: &str) -> Option<PathBuf> {
    let exact = directory.join(name);
    if exact.symlink_metadata().is_ok() {
        return Some(exact);
    }
    let wanted = name.to_lowercase();
    std::fs::read_dir(directory)
        .ok()?
        .flatten()
        .find(|entry| {
            let text = entry.file_name();
            let text = text.to_string_lossy();
            text.to_lowercase() == wanted
                || String::from_utf16_lossy(&short_name(&text.encode_utf16().collect::<Vec<_>>()))
                    .to_lowercase()
                    == wanted
        })
        .map(|entry| entry.path())
}

/// Wine's hash_short_file_name, used when a host name needs a DOS alias.
pub(crate) fn short_name(name: &[u16]) -> Vec<u16> {
    let invalid = |unit: u16| unit > 0x7f || b"*?<>|\"+=,;[] \xe5:/\\".contains(&(unit as u8));
    let dots: Vec<usize> = name
        .iter()
        .enumerate()
        .filter_map(|(i, &c)| (c == 46).then_some(i))
        .collect();
    let legal = name.len() <= 12
        && !name.iter().copied().any(invalid)
        && if name.first() == Some(&46) {
            name == [46] || name == [46, 46]
        } else if let [dot] = dots[..] {
            dot <= 8 && (2..5).contains(&(name.len() - dot))
        } else {
            dots.is_empty() && name.len() <= 8
        };
    if legal || name.is_empty() {
        return Vec::new();
    }
    let lower = |unit: u16| {
        char::from_u32(unit.into())
            .and_then(|value| value.to_lowercase().next())
            .map_or(unit, |value| value as u16)
    };
    let mut hash = 0xbeefu16;
    for (index, &unit) in name.iter().enumerate() {
        hash = (hash << 3)
            ^ (hash >> 5)
            ^ lower(unit)
            ^ name.get(index + 1).map_or(0, |&next| lower(next) << 8);
    }
    let first = name.iter().position(|&c| c != 46).unwrap_or(name.len());
    let extension = (first + 1..name.len().saturating_sub(1))
        .rev()
        .find(|&i| name[i] == 46);
    let replace = |unit: u16| {
        if invalid(unit) || unit == 46 || unit == 126 {
            95
        } else {
            (unit as u8).to_ascii_uppercase() as u16
        }
    };
    let mut result: Vec<u16> = name[..extension.unwrap_or(name.len())]
        .iter()
        .copied()
        .filter(|&c| c != 46)
        .take(4)
        .map(replace)
        .collect();
    result.resize(5, 126);
    for shift in [10, 5, 0] {
        result.push(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"[((hash >> shift) & 31) as usize] as u16);
    }
    if let Some(extension) = extension {
        result.push(46);
        result.extend(name[extension + 1..].iter().copied().take(3).map(replace));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    // spec: WINE-SYS-5, WINE-SYS-11
    #[test]
    fn drive_paths_resolve_without_regard_to_case() {
        let root = std::env::temp_dir().join(format!("adv-nt-path-{}", std::process::id()));
        let system = root.join("drive_c/windows/System32");
        std::fs::create_dir_all(&system).unwrap();
        std::fs::create_dir_all(root.join("dosdevices")).unwrap();
        std::os::unix::fs::symlink("../drive_c", root.join("dosdevices/c:")).unwrap();
        let devices = root.join("dosdevices");

        // A case-insensitive host file system finds the name as written.
        let Lookup::Found(found) = lookup(&devices, &utf16(r"\??\C:\WINDOWS\system32\")).unwrap()
        else {
            panic!("not found")
        };
        let expected = devices.join("c:/windows/System32");
        assert_eq!(
            found.to_string_lossy().to_lowercase(),
            expected.to_string_lossy().to_lowercase()
        );
        assert_eq!(
            lookup(&devices, &utf16(r"\??\c:\windows\.\new.txt")),
            Err(status::OBJECT_NAME_INVALID)
        );
        let missing = lookup(&devices, &utf16(r"\??\c:\windows\new.txt")).unwrap();
        assert_eq!(missing, Lookup::Missing(devices.join("c:/windows/new.txt")));
        assert_eq!(
            lookup(&devices, &utf16(r"\??\c:\none\file")),
            Err(status::OBJECT_PATH_NOT_FOUND)
        );
        assert_eq!(
            lookup(&devices, &utf16(r"\??\c:\a*b")),
            Err(status::OBJECT_NAME_INVALID)
        );
        assert_eq!(
            lookup(&devices, &utf16(r"\Device\Null")),
            Ok(Lookup::Device)
        );

        let unix = format!(r"\??\unix{}", system.to_string_lossy().replace('/', "\\"));
        assert_eq!(
            lookup(&devices, &utf16(&unix)),
            Ok(Lookup::Found(system.clone()))
        );
        assert_eq!(nt_name(&devices, &system), r"\??\C:\windows\System32");
        let outside = root.join("outside");
        let expected = format!(
            r"\??\unix\{}",
            root.canonicalize()
                .unwrap()
                .join("outside")
                .strip_prefix("/")
                .unwrap()
                .to_string_lossy()
                .replace('/', "\\")
        );
        assert_eq!(nt_name(&devices, &outside), expected);
        std::fs::remove_dir_all(root).unwrap();
    }
}
