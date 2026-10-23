//! NT file names to host paths, after Wine's `nt_to_unix_file_name`.
//!
//! A name of `\??\`, a drive letter and a colon, then the path on that
//! drive, names a file under `<prefix>/dosdevices/<letter>:`. Each
//! component is looked up as written first and then without regard to case,
//! as on Windows; `.` and `..` are resolved first, never above the drive.
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
    let mut parts = rest.split(['\\', '/']);
    let drive = parts.next().unwrap_or_default();
    let mut path = if drive == UNIX {
        PathBuf::from("/")
    } else if is_drive(drive) {
        let root = dosdevices.join(drive.to_ascii_lowercase());
        if !root.is_dir() {
            return Err(status::OBJECT_PATH_NOT_FOUND);
        }
        root
    } else {
        return Ok(Lookup::Device);
    };

    let mut components: Vec<&str> = Vec::new();
    for part in parts {
        match part {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            part if part.contains(INVALID) || part.chars().any(|c| c < ' ') => {
                return Err(status::OBJECT_NAME_INVALID);
            }
            part => components.push(part),
        }
    }

    let last = components.len();
    for (index, component) in components.into_iter().enumerate() {
        match find(&path, component) {
            Some(found) => path = found,
            None if index + 1 == last => return Ok(Lookup::Missing(path.join(component))),
            None => return Err(status::OBJECT_PATH_NOT_FOUND),
        }
    }
    Ok(Lookup::Found(path))
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
        .find(|entry| entry.file_name().to_str().map(str::to_lowercase) == Some(wanted.clone()))
        .map(|entry| entry.path())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

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
        let missing = lookup(&devices, &utf16(r"\??\c:\windows\.\x\..\new.txt")).unwrap();
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
