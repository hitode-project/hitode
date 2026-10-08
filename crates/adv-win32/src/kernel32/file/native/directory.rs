//! Directory fronts compose the same observable NT file operations as files.
use super::*;
use operations::{Done, close, open_existing};

pub(crate) fn error(status: u32) -> u32 {
    if status == 0xc0000035 {
        183
    } else {
        win32_error(status)
    }
}

pub(crate) fn create(env: &mut Env<'_>, path: String, then: Done<()>) -> Outcome {
    if path.is_empty() {
        return then(env, Err(0xc000003a));
    }
    operations::create(
        env,
        &path,
        0x100001,
        3,
        2,
        0x204021,
        Box::new(move |env, result| match result {
            Ok(handle) => close(env, handle, Box::new(move |env, _closed| then(env, Ok(())))),
            Err(status) => then(env, Err(status)),
        }),
    )
}

pub(crate) fn remove(env: &mut Env<'_>, path: String, then: Done<()>) -> Outcome {
    if path.is_empty() {
        return then(env, Err(0xc000003a));
    }
    let full = env.ext.get::<Win32>().vfs.normalize(&path);
    if full
        .trim_end_matches('\\')
        .eq_ignore_ascii_case(env.ext.get::<Win32>().vfs.cwd.trim_end_matches('\\'))
    {
        return then(env, Err(0xc0000043));
    }
    #[cfg(not(windows))]
    if !super::metadata::active(
        env,
        &[
            "NtCreateFile",
            "NtOpenFile",
            "NtSetInformationFile",
            "NtClose",
        ],
    ) {
        let vfs = &env.ext.get::<Win32>().vfs;
        if vfs.is_optical_path(&full) {
            return then(env, Err(0xc00000a2));
        }
        if vfs.has_read_only_underlay(&full) {
            warn_unavailable(
                env,
                "kernel32.dll!RemoveDirectory",
                "directory mutation over read-only VFS layers requires persistent whiteouts",
            );
            return then(env, Err(0xc00000bb));
        }
        let Some(host) = vfs.lookup(&full) else {
            return then(env, Err(0xc0000034));
        };
        let result = std::fs::remove_dir(host).map_err(|e| match e.kind() {
            std::io::ErrorKind::DirectoryNotEmpty => 0xc0000101,
            std::io::ErrorKind::NotADirectory => 0xc0000103,
            std::io::ErrorKind::PermissionDenied => 0xc0000022,
            _ => 0xc0000001,
        });
        return then(env, result);
    }
    open_existing(
        env,
        &full,
        0x110080,
        7,
        0x204021,
        Box::new(move |env, opened| {
            let handle = match opened {
                Ok(handle) => handle,
                Err(status) => return then(env, Err(status)),
            };
            super::mutation::set_disposition(
                env,
                handle,
                Box::new(move |env, changed| {
                    close(
                        env,
                        handle,
                        Box::new(move |env, _closed| then(env, changed)),
                    )
                }),
            )
        }),
    )
}

pub(in crate::kernel32) fn win32(env: &mut Env<'_>, path: String, removing: bool) -> Outcome {
    let then: Done<()> = Box::new(|env, result| match result {
        Ok(()) => Outcome::Return(1),
        Err(status) => {
            crate::util::set_last_error(env, error(status));
            Outcome::Return(0)
        }
    });
    if removing {
        remove(env, path, then)
    } else {
        create(env, path, then)
    }
}

#[cfg(not(windows))]
pub(crate) fn portable_rename(env: &mut Env<'_>, source: String, target: String) -> Outcome {
    let error = |env: &mut Env<'_>, code| {
        crate::util::set_last_error(env, code);
        Outcome::Return(0)
    };
    let vfs = &env.ext.get::<Win32>().vfs;
    let source = vfs.normalize(&source);
    let target = vfs.normalize(&target);
    if vfs.is_optical_path(&source) || vfs.is_optical_path(&target) {
        return error(env, 19);
    }
    if vfs.has_read_only_underlay(&source) || vfs.has_read_only_underlay(&target) {
        warn_unavailable(
            env,
            "kernel32.dll!MoveFile",
            "directory mutation over read-only VFS layers requires persistent whiteouts",
        );
        return error(env, 50);
    }
    let volume = |path: &str| {
        if let Some(unc) = path.strip_prefix(r"\\") {
            unc.split('\\')
                .take(2)
                .collect::<Vec<_>>()
                .join("\\")
                .to_ascii_lowercase()
        } else {
            path.get(..2).unwrap_or("").to_ascii_lowercase()
        }
    };
    if volume(&source) != volume(&target) {
        return error(env, 17);
    }
    if vfs.lookup(&target).is_some() && !source.eq_ignore_ascii_case(&target) {
        return error(env, 183);
    }
    if target
        .get(..source.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&source))
        && target.as_bytes().get(source.len()) == Some(&b'\\')
    {
        return error(env, 87);
    }
    let parent = target.rsplit_once('\\').map_or("", |(parent, _)| parent);
    let parent = if parent.len() == 2 {
        format!("{parent}\\")
    } else {
        parent.into()
    };
    if !vfs.is_dir(&parent) {
        return error(env, 3);
    }
    let Some(from) = vfs.lookup(&source) else {
        return error(env, 2);
    };
    if !from.is_dir() {
        return error(env, 267);
    }
    let Some(mut to) = vfs.write_path(&target) else {
        return error(env, 3);
    };
    if let Some(leaf) = target.rsplit('\\').next() {
        to.set_file_name(leaf);
    }
    match std::fs::rename(from, to) {
        Ok(()) => {
            crate::ntdll::file::directory::retarget_owners(env, &source, &target);
            Outcome::Return(1)
        }
        Err(error) => {
            let code = crate::kernel32::file::io_code(&error);
            crate::util::set_last_error(env, code);
            Outcome::Return(0)
        }
    }
}
