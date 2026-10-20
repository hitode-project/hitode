//! Flatten profile-based save folders in portable mode, retaining original backups.

use super::{ci_find, ci_path};
use crate::OsConfig;
use std::io;
use std::path::Path;

const MARKER: &str = "portable-saves-v1";

/// Preserve the subtree beginning at `save`, only inside document/application-data
/// roots. Windows files and arbitrary absolute paths must never alias game files.
pub(super) fn relative<'a>(parts: &'a [String], user: &str) -> Option<&'a [String]> {
    let is = |i: usize, s: &str| parts.get(i).is_some_and(|p| p.eq_ignore_ascii_case(s));
    let start = if is(0, "Users") && (is(1, user) || is(1, "Public")) && is(2, "Documents") {
        3
    } else if is(0, "Users")
        && is(1, user)
        && is(2, "AppData")
        && (is(3, "Roaming") || is(3, "Local"))
    {
        4
    } else if is(0, "ProgramData") {
        1
    } else {
        return None;
    };
    let offset = parts
        .get(start..)?
        .iter()
        .position(|p| p.eq_ignore_ascii_case("save"))?;
    Some(&parts[start + offset..])
}

pub(super) fn migrate(cfg: &OsConfig) -> io::Result<()> {
    let marker = cfg.data_dir.join(MARKER);
    if marker.is_file() {
        return Ok(());
    }
    let legacy = cfg.data_dir.join("c");
    let roots = [
        vec!["Users", &cfg.user_name, "Documents"],
        vec!["Users", &cfg.user_name, "AppData", "Roaming"],
        vec!["Users", &cfg.user_name, "AppData", "Local"],
        vec!["Users", "Public", "Documents"],
        vec!["ProgramData"],
    ];
    let mut found = false;
    for root in roots {
        let parts = root.into_iter().map(str::to_owned).collect::<Vec<_>>();
        if let Some(path) = ci_find(&legacy, &parts) {
            found |= find_saves(&path, &cfg.game_dir)?;
        }
    }
    if found {
        // Only after all copies succeed. Never import these backups again: otherwise
        // deleting a slot in the new layout could resurrect it on the next launch.
        super::host::write(
            &marker,
            b"Legacy profile saves imported; game directory wins conflicts.\n",
        )?;
    }
    Ok(())
}

fn find_saves(dir: &Path, game: &Path) -> io::Result<bool> {
    let mut found = false;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_symlink() {
            return Err(io::Error::other(
                "legacy save tree contains a symbolic link",
            ));
        }
        if ty.is_dir() {
            if entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case("save")
            {
                let target = ci_path(game, &["save".into()]);
                copy_missing(&entry.path(), &target)?;
                found = true;
            } else {
                found |= find_saves(&entry.path(), game)?;
            }
        }
    }
    Ok(found)
}

fn copy_missing(source: &Path, target: &Path) -> io::Result<()> {
    super::host::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let dst = ci_path(target, &[name]);
        if ty.is_symlink() {
            return Err(io::Error::other(
                "legacy save tree contains a symbolic link",
            ));
        } else if ty.is_dir() {
            copy_missing(&entry.path(), &dst)?;
        } else if ty.is_file() {
            let mut input = std::fs::File::open(entry.path())?;
            let mut output = match super::host::open(
                std::fs::OpenOptions::new().write(true).create_new(true),
                &dst,
            ) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dst.is_file() => continue,
                Err(e) => return Err(e),
            };
            if let Err(e) = io::copy(&mut input, &mut output).and_then(|_| output.sync_all()) {
                drop(output);
                let _ = super::host::remove_file(&dst);
                return Err(e);
            }
        }
    }
    Ok(())
}
