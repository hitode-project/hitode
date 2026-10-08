//! Path mutations compose observable open/set/close over one native handle.
use super::*;
use operations::{Done, close, open_existing, set_information};

fn result(env: &mut Env<'_>, class: u32, status: Result<(), u32>) -> Outcome {
    match status {
        Ok(()) => Outcome::Return(1),
        Err(status) => {
            crate::util::set_last_error(
                env,
                if class == 10 && status == 0xc0000035 {
                    183
                } else {
                    win32_error(status)
                },
            );
            Outcome::Return(0)
        }
    }
}

pub(in crate::kernel32) fn delete(env: &mut Env<'_>, path: String) -> Outcome {
    #[cfg(not(windows))]
    if portable_active(env) {
        return portable(env, path, None);
    }
    mutate(env, path, 64, vec![])
}

/// Modern Win32 uses POSIX disposition first. NT_SUCCESS is final; every
/// negative status except CANNOT_DELETE retries the classic BOOLEAN request.
pub(crate) fn set_disposition(env: &mut Env<'_>, handle: u32, then: Done<()>) -> Outcome {
    set_information(
        env,
        handle,
        64,
        3u32.to_le_bytes().to_vec(),
        Box::new(move |env, result| match result {
            Err(status) if status != 0xc0000121 => set_information(env, handle, 13, vec![1], then),
            result => then(env, result),
        }),
    )
}

pub(in crate::kernel32) fn rename(env: &mut Env<'_>, source: String, target: String) -> Outcome {
    let target = env.ext.get::<Win32>().vfs.normalize(&target);
    #[cfg(not(windows))]
    if portable_active(env) {
        return portable(env, source, Some(target));
    }
    let name = if let Some(unc) = target.strip_prefix(r"\\") {
        format!(r"\??\UNC\{unc}")
    } else {
        format!(r"\??\{target}")
    };
    let name: Vec<_> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut input = vec![0; 12]; // x86 FILE_RENAME_INFORMATION, no replacement/root.
    input[8..12].copy_from_slice(&(name.len() as u32).to_le_bytes());
    input.extend(name);
    mutate(env, source, 10, input)
}

#[cfg(not(windows))]
fn portable_active(env: &mut Env<'_>) -> bool {
    // Preserve the existing portable path backend only while every observable
    // native operation is pristine. A changed entry always owns the result.
    !super::metadata::active(
        env,
        &[
            "NtCreateFile",
            "NtOpenFile",
            "NtSetInformationFile",
            "NtClose",
        ],
    )
}

#[cfg(not(windows))]
fn portable(env: &mut Env<'_>, source: String, target: Option<String>) -> Outcome {
    use crate::{FileObj, util::set_last_error};
    let source = env.ext.get::<Win32>().vfs.normalize(&source);
    let error = |env: &mut Env<'_>, code| {
        set_last_error(env, code);
        Outcome::Return(0)
    };
    let vfs = &env.ext.get::<Win32>().vfs;
    if vfs.is_optical_path(&source) {
        return error(env, 19);
    }
    if vfs.has_read_only_underlay(&source) {
        warn_unavailable(
            env,
            "kernel32.dll!<file mutation>",
            "deleting or renaming read-only VFS layers requires persistent whiteouts",
        );
        return error(env, 50);
    }
    let Some(from) = vfs.lookup(&source) else {
        return error(env, 2);
    };
    if !from.is_file() {
        if target.is_some() {
            warn_unavailable(
                env,
                "kernel32.dll!MoveFile",
                "directory rename owners are unavailable",
            );
            return error(env, 50);
        }
        return error(env, 5);
    }
    let identity =
        match std::fs::File::open(&from).and_then(|file| adv_os::vfs::file_identity(&file)) {
            Ok(identity) => identity,
            Err(e) => return error(env, crate::kernel32::file::io_code(&e)),
        };
    if env
        .sys
        .objects
        .all_hosts::<FileObj>()
        .any(|(_, file)| file.file.cached_identity() == Some(identity) && file.share & 4 == 0)
    {
        return error(env, 32);
    }
    let changed = if let Some(target) = target {
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
        if vfs.has_read_only_underlay(&target) {
            warn_unavailable(
                env,
                "kernel32.dll!MoveFile",
                "renaming read-only VFS layers requires persistent whiteouts",
            );
            return error(env, 50);
        }
        let parent = target.rsplit_once('\\').map_or("", |(parent, _)| parent);
        let parent = if parent.len() == 2 {
            format!(r"{parent}\")
        } else {
            parent.into()
        };
        if !vfs.is_dir(&parent) {
            return error(env, 3);
        }
        let Some(mut to) = vfs.write_path(&target) else {
            return error(env, 3);
        };
        if let Some(leaf) = target.rsplit('\\').next() {
            to.set_file_name(leaf);
        }
        std::fs::rename(from, to).map(|()| Some(target))
    } else {
        std::fs::remove_file(from).map(|()| None)
    };
    match changed {
        Ok(target) => {
            if let Some(target) = target {
                for (_, file) in env.sys.objects.all_hosts_mut::<FileObj>() {
                    if file.file.cached_identity() == Some(identity)
                        && file.guest_path.eq_ignore_ascii_case(&source)
                    {
                        file.guest_path = target.clone();
                    }
                }
            }
            Outcome::Return(1)
        }
        Err(e) => error(
            env,
            if e.raw_os_error() == Some(18) {
                17
            } else {
                crate::kernel32::file::io_code(&e)
            },
        ),
    }
}

fn mutate(env: &mut Env<'_>, path: String, class: u32, input: Vec<u8>) -> Outcome {
    let then: Done<u32> = Box::new(move |env, opened| {
        let handle = match opened {
            Ok(handle) => handle,
            Err(status) => return result(env, class, Err(status)),
        };
        let done: Done<()> = Box::new(move |env, changed| {
            close(
                env,
                handle,
                // Win32 decides the mutation result from NtSetInformationFile;
                // native cleanup still runs, but its status is not propagated.
                Box::new(move |env, _closed| result(env, class, changed)),
            )
        });
        if class == 64 {
            set_disposition(env, handle, done)
        } else {
            set_information(env, handle, class, input, done)
        }
    });
    // DELETE | SYNCHRONIZE | FILE_READ_ATTRIBUTES. Both independent open
    // objects and duplicated aliases retain their original backing identity.
    open_existing(
        env,
        &path,
        if class == 64 {
            0x0001_0080
        } else {
            0x0011_0080
        },
        7,
        if class == 10 {
            0x0020_4020
        } else {
            0x0020_4040
        },
        then,
    )
}

#[cfg(test)]
mod tests;
