//! Files, directories, and .ini profiles, all through the [`adv_os::Vfs`].

use std::io::{Read, Seek, SeekFrom, Write};
use std::time::SystemTime;

use adv_abi::Registry;
use adv_cpu::backend::{Guest, GuestExt};
use adv_machine::{Env, Outcome};

use super::K32;
use crate::util::{
    INVALID_HANDLE_VALUE, cstr, err, out_u32, put_cstr, put_wstr, set_last_error, wstr, zero,
};
use crate::{FileObj, FindObj, STD_ERROR, STD_INPUT, STD_OUTPUT, Win32, stdcall};
use adv_os::vfs::{DirEntry, wildcard};
mod attributes;
pub(crate) use attributes::file_attributes_path;
mod path;
mod profile;
#[cfg(test)]
mod tests;

const GENERIC_WRITE: u32 = 0x4000_0000;
const CREATE_NEW: u32 = 1;
const CREATE_ALWAYS: u32 = 2;
const OPEN_EXISTING: u32 = 3;
const OPEN_ALWAYS: u32 = 4;
const TRUNCATE_EXISTING: u32 = 5;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;

fn system_file(env: &mut Env<'_>, path: &str) -> Result<Option<std::sync::Arc<[u8]>>, ()> {
    let files = env
        .ext
        .get_or_default::<adv_os::system_files::SystemFiles>();
    files.open(env.cpu, env.sys, path).map_err(|error| {
        log::warn!("synthetic file {path}: {error}");
    })
}

fn path_a(env: &Env<'_>, p: u32) -> String {
    env.ext.get::<Win32>().codec.decode(&cstr(env.cpu, p))
}

fn path_w(env: &Env<'_>, p: u32) -> String {
    String::from_utf16_lossy(&wstr(env.cpu, p))
}

fn io_error(env: &mut Env<'_>, e: &std::io::Error) {
    set_last_error(env, io_code(e));
}

fn io_code(e: &std::io::Error) -> u32 {
    match e.kind() {
        std::io::ErrorKind::NotFound => err::FILE_NOT_FOUND,
        std::io::ErrorKind::PermissionDenied => err::ACCESS_DENIED,
        std::io::ErrorKind::AlreadyExists => err::FILE_EXISTS,
        _ => err::ACCESS_DENIED,
    }
}

/// Replacement preflight precedes write_path/copy-up and every target write.
/// Native CopyFile needs an unmapped destination even at unchanged length;
/// profile writes can update/grow it, but cannot shrink a live section.
fn replacement_allowed(env: &Env<'_>, path: &str, length: u64, unmapped: bool) -> Result<(), u32> {
    let vfs = &env.ext.get::<Win32>().vfs;
    if !vfs.sections.has_entries() {
        return Ok(());
    }
    let Some(path) = vfs.lookup(path).filter(|p| p.is_file()) else {
        return Ok(());
    };
    let file = std::fs::File::open(path).map_err(|e| io_code(&e))?;
    let identity = adv_os::vfs::file_identity(&file).map_err(|e| io_code(&e))?;
    if let Some(section) = vfs.sections.lookup(identity)
        && (unmapped
            || length < file.metadata().map_err(|e| io_code(&e))?.len()
            || adv_os::try_modules(env.ext)
                .and_then(|modules| modules.minimum_data_file_length(&section))
                .is_some_and(|minimum| length < minimum))
    {
        return Err(1224); // ERROR_USER_MAPPED_FILE
    }
    Ok(())
}

/// Open through the ordinary VFS copy-up/FD-retarget path without truncating.
/// Each successful write prefix is published before a later error is returned.
fn replace_contents(
    env: &mut Env<'_>,
    path: &str,
    length: u64,
    unmapped: bool,
    write: impl FnOnce(
        &mut adv_os::vfs::FileStream,
        Option<&adv_os::vfs::FileSection>,
    ) -> std::io::Result<()>,
) -> Result<(), u32> {
    replacement_allowed(env, path, length, unmapped)?;
    let handle = open(env, path, GENERIC_WRITE, OPEN_ALWAYS);
    if handle == INVALID_HANDLE_VALUE {
        return Err(crate::util::last_error(env));
    }
    let result = (|| {
        let file = &mut env.sys.objects.host_mut::<FileObj>(handle).unwrap().file;
        let sections = &env.ext.get::<Win32>().vfs.sections;
        let content = if sections.has_entries() {
            file.cached_identity().and_then(|id| sections.lookup(id))
        } else {
            None
        };
        file.seek(SeekFrom::Start(0))?;
        write(file, content.as_deref())?;
        if file.metadata()?.len() != length {
            file.set_len(length)?;
        }
        Ok::<_, std::io::Error>(())
    })();
    env.sys.objects.close(handle);
    result.map_err(|e| io_code(&e))
}

fn read_contents(env: &Env<'_>, guest: &str) -> std::io::Result<Vec<u8>> {
    let vfs = &env.ext.get::<Win32>().vfs;
    if vfs.is_optical_path(guest) {
        let mut bytes = Vec::new();
        vfs.open_optical_file(guest)?.read_to_end(&mut bytes)?;
        return Ok(bytes);
    }
    let Some(path) = vfs.lookup(guest) else {
        return Ok(Vec::new());
    };
    let mut file = std::fs::File::open(path)?;
    let content = if vfs.sections.has_entries() {
        vfs.sections.lookup(adv_os::vfs::file_identity(&file)?)
    } else {
        None
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if let Some(content) = content {
        content.merge_read(0, &mut bytes)?;
    }
    Ok(bytes)
}

fn create_file(env: &mut Env<'_>, path: String) -> Outcome {
    let (access, share, disposition) = (env.arg(1), env.arg(2), env.arg(4));
    let inherit = match super::handle::security_inherit(env, env.arg(3)) {
        Ok(inherit) => inherit,
        Err(code) => {
            set_last_error(env, code);
            return Outcome::Return(INVALID_HANDLE_VALUE);
        }
    };
    let handle = open_with_share(env, &path, access, disposition, share);
    if handle != INVALID_HANDLE_VALUE {
        super::handle::set_inherit(env, handle, inherit);
    }
    Outcome::Return(handle)
}

/// CreateFile's core for host callers (PAL's file API): open `path` with the given access
/// and creation disposition, register the handle, and set the last error. Returns the
/// handle or `INVALID_HANDLE_VALUE`.
pub fn open(env: &mut Env<'_>, path: &str, access: u32, disposition: u32) -> u32 {
    open_with_share(env, path, access, disposition, 7)
}

pub(crate) fn open_with_share(
    env: &mut Env<'_>,
    path: &str,
    access: u32,
    disposition: u32,
    share: u32,
) -> u32 {
    // Check ownership capacity before creating or truncating an overlay file.
    if !env.sys.objects.can_insert() {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
        return INVALID_HANDLE_VALUE;
    }
    let guest = env.ext.get::<Win32>().vfs.normalize(path);
    if let Some(device) = path
        .strip_prefix(r"\\.\")
        .or_else(|| path.strip_prefix(r"\\?\"))
        && device.len() == 2
        && device.as_bytes()[1] == b':'
        && env
            .ext
            .get::<Win32>()
            .vfs
            .optical_drive(device.as_bytes()[0] as char)
            .is_some()
    {
        adv_abi::warn_unavailable(
            env,
            "kernel32.dll!CreateFile",
            "raw optical device handles are not modeled; mounted image files are available",
        );
        set_last_error(env, 50);
        return INVALID_HANDLE_VALUE;
    }
    if share & !7 != 0 {
        set_last_error(env, err::INVALID_PARAMETER);
        return INVALID_HANDLE_VALUE;
    }
    let share_access = |rights: u32| {
        u32::from(rights & (0x80000000 | 0x10000000 | 1 | 0x20) != 0)
            | (u32::from(rights & (GENERIC_WRITE | 0x10000000 | 2 | 4) != 0) << 1)
            | (u32::from(rights & (0x10000 | 0x10000000) != 0) << 2)
    };
    let key = guest.to_lowercase();
    if env.sys.objects.all_hosts::<FileObj>().any(|(_, file)| {
        file.guest_path.to_lowercase() == key
            && (share_access(access) & !file.share != 0 || share_access(file.access) & !share != 0)
    }) {
        set_last_error(env, 32); // ERROR_SHARING_VIOLATION, before opening/truncating.
        return INVALID_HANDLE_VALUE;
    }
    if env.ext.get::<Win32>().vfs.is_optical_path(&guest) {
        if access & (GENERIC_WRITE | 0x10000000 | 0x10000 | 2 | 4) != 0
            || !matches!(disposition, OPEN_EXISTING | OPEN_ALWAYS | CREATE_NEW)
        {
            set_last_error(env, 19); // ERROR_WRITE_PROTECT
            return INVALID_HANDLE_VALUE;
        }
        match env.ext.get::<Win32>().vfs.open_optical_file(&guest) {
            Ok(_) if disposition == CREATE_NEW => {
                set_last_error(env, err::FILE_EXISTS);
                return INVALID_HANDLE_VALUE;
            }
            Ok(file) => {
                let handle = env.sys.objects.insert_host(FileObj {
                    file,
                    guest_path: guest,
                    access,
                    share,
                });
                set_last_error(
                    env,
                    if disposition == OPEN_ALWAYS {
                        err::ALREADY_EXISTS
                    } else {
                        0
                    },
                );
                return handle;
            }
            Err(error) => {
                io_error(env, &error);
                return INVALID_HANDLE_VALUE;
            }
        }
    }
    match system_file(env, &guest) {
        Ok(Some(bytes)) => {
            if disposition == CREATE_NEW {
                set_last_error(env, err::FILE_EXISTS);
                return INVALID_HANDLE_VALUE;
            }
            if access & (GENERIC_WRITE | 0x1000_0000 | 0x10000) != 0
                || !matches!(disposition, OPEN_EXISTING | OPEN_ALWAYS)
            {
                set_last_error(env, err::ACCESS_DENIED);
                return INVALID_HANDLE_VALUE;
            }
            let handle = env.sys.objects.insert_host(FileObj {
                file: adv_os::vfs::FileStream::read_only(bytes),
                guest_path: guest,
                access,
                share,
            });
            set_last_error(
                env,
                if disposition == OPEN_ALWAYS {
                    err::ALREADY_EXISTS
                } else {
                    0
                },
            );
            return handle;
        }
        Err(()) => {
            set_last_error(env, err::ACCESS_DENIED);
            return INVALID_HANDLE_VALUE;
        }
        Ok(None) => {}
    }
    let w = env.ext.get::<Win32>();
    let existing = w.vfs.lookup(&guest);
    if matches!(disposition, CREATE_ALWAYS | TRUNCATE_EXISTING)
        && w.vfs.sections.has_entries()
        && let Some(path) = existing.as_ref().filter(|path| path.is_file())
    {
        let identity =
            match std::fs::File::open(path).and_then(|file| adv_os::vfs::file_identity(&file)) {
                Ok(identity) => identity,
                Err(error) => {
                    io_error(env, &error);
                    return INVALID_HANDLE_VALUE;
                }
            };
        if let Some(section) = w.vfs.sections.lookup(identity)
            && adv_os::try_modules(env.ext)
                .and_then(|modules| modules.minimum_data_file_length(&section))
                .is_some()
        {
            // Reject before write_path, copy-up, FD retarget or truncating open.
            set_last_error(env, 1224); // ERROR_USER_MAPPED_FILE
            return INVALID_HANDLE_VALUE;
        }
    }
    let write = access & GENERIC_WRITE != 0
        || matches!(disposition, CREATE_NEW | CREATE_ALWAYS | TRUNCATE_EXISTING);
    let exists = existing.as_ref().is_some_and(|p| p.is_file());
    let mut transfer = None;

    let result = if !write {
        match (&existing, disposition) {
            (Some(p), _) if p.is_file() => std::fs::File::open(p),
            (_, OPEN_ALWAYS) => {
                w.vfs
                    .write_path(&guest)
                    .map_or(Err(std::io::ErrorKind::NotFound.into()), |p| {
                        std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create(true)
                            .truncate(false)
                            .open(p)
                    })
            }
            _ => Err(std::io::ErrorKind::NotFound.into()),
        }
    } else {
        if disposition == CREATE_NEW && exists {
            set_last_error(env, err::FILE_EXISTS);
            return INVALID_HANDLE_VALUE;
        }
        if matches!(disposition, OPEN_EXISTING | TRUNCATE_EXISTING) && !exists {
            set_last_error(env, err::FILE_NOT_FOUND);
            return INVALID_HANDLE_VALUE;
        }
        let truncate = matches!(disposition, CREATE_ALWAYS | TRUNCATE_EXISTING);
        match w.vfs.write_path(&guest) {
            None => Err(std::io::ErrorKind::NotFound.into()),
            Some(dst) => {
                // Copy-on-write from the selected read-only game/replacement layer.
                if !truncate
                    && w.vfs.is_base_only(&guest)
                    && let Some(src) = &existing
                {
                    let old = match std::fs::File::open(src)
                        .and_then(|file| adv_os::vfs::file_identity(&file))
                    {
                        Ok(id) => id,
                        Err(error) => {
                            io_error(env, &error);
                            return INVALID_HANDLE_VALUE;
                        }
                    };
                    if let Err(error) = std::fs::copy(src, &dst) {
                        io_error(env, &error);
                        return INVALID_HANDLE_VALUE;
                    }
                    transfer = Some((old, dst.clone()));
                }
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(truncate)
                    .open(dst)
            }
        }
    };
    match result {
        Ok(file) => {
            if let Some((old, destination)) = transfer {
                let replacements = match super::file_mapping::prepare_retarget_handles(
                    env,
                    &guest,
                    old,
                    &destination,
                ) {
                    Ok(replacements) => replacements,
                    Err(error) => {
                        set_last_error(env, error);
                        return INVALID_HANDLE_VALUE;
                    }
                };
                let content = match file.try_clone().and_then(|owned| {
                    let len = owned.metadata()?.len();
                    env.ext
                        .get::<Win32>()
                        .vfs
                        .sections
                        .transfer(old, owned, len, true)
                }) {
                    Ok(content) => content,
                    Err(error) => {
                        io_error(env, &error);
                        return INVALID_HANDLE_VALUE;
                    }
                };
                replacements.apply(&mut env.sys.objects);
                // The weak registry must not extend content lifetime on its own.
                // Existing views keep it live; with no view this temporary drops.
                drop(content);
            }
            let h = env.sys.objects.insert_host(FileObj {
                file: file.into(),
                guest_path: guest.clone(),
                access,
                share,
            });
            log::debug!("CreateFile({guest}, {access:#x}, {disposition}) = {h:#x}");
            // ERROR_ALREADY_EXISTS is reported for CREATE_ALWAYS / OPEN_ALWAYS on success.
            let code = if exists && matches!(disposition, CREATE_ALWAYS | OPEN_ALWAYS) {
                err::ALREADY_EXISTS
            } else {
                0
            };
            set_last_error(env, code);
            h
        }
        Err(e) => {
            log::debug!("CreateFile({guest}) failed: {e}");
            io_error(env, &e);
            INVALID_HANDLE_VALUE
        }
    }
}

/// Read the shared stream cursor, then merge dirty mapped-file content over
/// the bytes obtained from the backing file. All API fronts use this owner.
pub(crate) fn read_coherent(
    f: &mut FileObj,
    section: &adv_os::vfs::SharedFiles,
    data: &mut [u8],
) -> std::io::Result<usize> {
    let content = if section.has_entries() {
        f.file.cached_identity().and_then(|id| section.lookup(id))
    } else {
        None
    };
    let coherence = if let Some(content) = content {
        Some((content, f.file.stream_position()?))
    } else {
        None
    };
    let mut got = 0;
    while got < data.len() {
        match f.file.read(&mut data[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(e) => return Err(e),
        }
    }
    if let Some((content, offset)) = coherence
        && let Err(error) = content.merge_read(offset, &mut data[..got])
    {
        return Err(error);
    }
    Ok(got)
}

fn read_file(env: &mut Env<'_>) -> Outcome {
    let (h, buf, n, read_out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let Some(f) = env.sys.objects.host_mut::<FileObj>(h) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    let mut data = vec![0u8; n as usize];
    let got = match read_coherent(f, &env.ext.get::<Win32>().vfs.sections, &mut data) {
        Ok(got) => got,
        Err(error) => {
            io_error(env, &error);
            return Outcome::Return(0);
        }
    };
    if env.cpu.write(buf, &data[..got]).is_err() {
        set_last_error(env, 998); // ERROR_NOACCESS
        return Outcome::Return(0);
    }
    out_u32(env.cpu, read_out, got as u32);
    Outcome::Return(1)
}

fn write_file(env: &mut Env<'_>) -> Outcome {
    let (h, buf, n, written) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let mut data = vec![0u8; n as usize];
    let _ = env.cpu.read(buf, &mut data);
    if matches!(h, STD_OUTPUT | STD_ERROR) {
        let s = env.ext.get::<Win32>().codec.decode(&data);
        log::info!("[guest stdout] {}", s.trim_end());
        out_u32(env.cpu, written, n);
        return Outcome::Return(1);
    }
    let Some(f) = env.sys.objects.host_mut::<FileObj>(h) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    let section = &env.ext.get::<Win32>().vfs.sections;
    let content = if section.has_entries() {
        f.file.cached_identity().and_then(|id| section.lookup(id))
    } else {
        None
    };
    let content = if let Some(content) = content {
        match f.file.stream_position() {
            Ok(offset) => Some((content, offset)),
            Err(error) => {
                io_error(env, &error);
                return Outcome::Return(0);
            }
        }
    } else {
        None
    };
    let result = if let Some((content, offset)) = content {
        content.write_to(&mut f.file, offset, &data)
    } else {
        f.file.write_all(&data)
    };
    match result {
        Ok(()) => {
            out_u32(env.cpu, written, n);
            Outcome::Return(1)
        }
        Err(error) => {
            io_error(env, &error);
            Outcome::Return(0)
        }
    }
}

fn set_end_of_file(env: &mut Env<'_>) -> Outcome {
    let Some(file) = env.sys.objects.host_mut::<FileObj>(env.arg(0)) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    let position = match file.file.stream_position() {
        Ok(position) => position,
        Err(error) => {
            io_error(env, &error);
            return Outcome::Return(0);
        }
    };
    let section = file
        .file
        .cached_identity()
        .and_then(|identity| env.ext.get::<Win32>().vfs.sections.lookup(identity));
    if let Some(section) = section
        && adv_os::try_modules(env.ext)
            .and_then(|modules| modules.minimum_data_file_length(&section))
            .is_some_and(|minimum| position < minimum)
    {
        set_last_error(env, 1224);
        return Outcome::Return(0);
    }
    match file.file.set_len(position) {
        Ok(()) => Outcome::Return(1),
        Err(error) => {
            io_error(env, &error);
            Outcome::Return(0)
        }
    }
}

/// Move `h`'s file pointer; the new position, or `None` with the last error set.
fn seek(env: &mut Env<'_>, h: u32, dist: i64, method: u32) -> Option<u64> {
    let Some(f) = env.sys.objects.host_mut::<FileObj>(h) else {
        set_last_error(env, err::INVALID_HANDLE);
        return None;
    };
    let pos = match method {
        0 => SeekFrom::Start(dist.max(0) as u64),
        1 => SeekFrom::Current(dist),
        _ => SeekFrom::End(dist),
    };
    match f.file.seek(pos) {
        Ok(p) => {
            set_last_error(env, 0);
            Some(p)
        }
        Err(e) => {
            io_error(env, &e);
            None
        }
    }
}

fn set_file_pointer(env: &mut Env<'_>) -> Outcome {
    let (h, lo, hi_p, method) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let hi = if hi_p != 0 {
        env.cpu.read_u32(hi_p).unwrap_or(0)
    } else if (lo as i32) < 0 {
        u32::MAX
    } else {
        0
    };
    let dist = ((u64::from(hi) << 32) | u64::from(lo)) as i64;
    match seek(env, h, dist, method) {
        Some(p) => {
            out_u32(env.cpu, hi_p, (p >> 32) as u32);
            Outcome::Return(p as u32)
        }
        None => Outcome::Return(u32::MAX),
    }
}

/// SetFilePointerEx(h, LARGE_INTEGER distance (two stack words), new_pos*, method).
fn set_file_pointer_ex(env: &mut Env<'_>) -> Outcome {
    let (h, lo, hi, out, method) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let dist = ((u64::from(hi) << 32) | u64::from(lo)) as i64;
    match seek(env, h, dist, method) {
        Some(p) => {
            out_u32(env.cpu, out, p as u32);
            if out != 0 {
                out_u32(env.cpu, out + 4, (p >> 32) as u32);
            }
            Outcome::Return(1)
        }
        None => Outcome::Return(0),
    }
}

fn file_size(env: &mut Env<'_>, h: u32) -> Option<u64> {
    env.sys
        .objects
        .host::<FileObj>(h)
        .and_then(|f| f.file.metadata().ok())
        .map(|m| m.len())
}

fn get_file_size(env: &mut Env<'_>) -> Outcome {
    let (h, hi_p) = (env.arg(0), env.arg(1));
    match file_size(env, h) {
        Some(s) => {
            out_u32(env.cpu, hi_p, (s >> 32) as u32);
            Outcome::Return(s as u32)
        }
        None => {
            set_last_error(env, err::INVALID_HANDLE);
            Outcome::Return(u32::MAX)
        }
    }
}

fn get_file_size_ex(env: &mut Env<'_>) -> Outcome {
    let (h, p) = (env.arg(0), env.arg(1));
    match file_size(env, h) {
        Some(s) => {
            let _ = env.cpu.write(p, &s.to_le_bytes());
            Outcome::Return(1)
        }
        None => Outcome::Return(0),
    }
}

fn get_file_type(env: &mut Env<'_>) -> Outcome {
    let h = env.arg(0);
    Outcome::Return(match h {
        STD_INPUT | STD_OUTPUT | STD_ERROR => 2, // FILE_TYPE_CHAR
        _ if env.sys.objects.host::<FileObj>(h).is_some() => 1,
        _ => 0,
    })
}

fn filetime(t: Option<SystemTime>) -> u64 {
    let d = t
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .unwrap_or_default();
    116_444_736_000_000_000 + d.as_micros() as u64 * 10
}

fn write_filetime(cpu: &mut dyn Guest, p: u32, ft: u64) {
    if p != 0 {
        let _ = cpu.write(p, &ft.to_le_bytes());
    }
}

fn get_file_time(env: &mut Env<'_>) -> Outcome {
    let (h, c, a, m) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let Some(md) = env
        .sys
        .objects
        .host::<FileObj>(h)
        .and_then(|f| f.file.metadata().ok())
    else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    write_filetime(
        env.cpu,
        c,
        filetime(md.created().ok().or(md.modified().ok())),
    );
    write_filetime(env.cpu, a, filetime(md.accessed().ok()));
    write_filetime(env.cpu, m, filetime(md.modified().ok()));
    Outcome::Return(1)
}

pub(super) fn delete_file(env: &mut Env<'_>, path: String) -> Outcome {
    let guest = env.ext.get::<Win32>().vfs.normalize(&path);
    if matches!(system_file(env, &guest), Ok(Some(_))) {
        set_last_error(env, err::ACCESS_DENIED);
        return Outcome::Return(0);
    }
    let w = env.ext.get::<Win32>();
    if w.vfs.is_base_only(&path) {
        log::warn!("DeleteFile({path}): refusing to delete a game file");
        set_last_error(env, err::ACCESS_DENIED);
        return Outcome::Return(0);
    }
    match w.vfs.lookup(&path).map(std::fs::remove_file) {
        Some(Ok(())) => Outcome::Return(1),
        Some(Err(e)) => {
            io_error(env, &e);
            Outcome::Return(0)
        }
        None => {
            set_last_error(env, err::FILE_NOT_FOUND);
            Outcome::Return(0)
        }
    }
}

pub(super) fn copy_file(
    env: &mut Env<'_>,
    src: String,
    dst: String,
    fail_if_exists: bool,
    remove_src: bool,
) -> Outcome {
    let w = env.ext.get::<Win32>();
    let optical = w.vfs.is_optical_path(&src);
    if w.vfs.is_optical_path(&dst) || (remove_src && optical) {
        set_last_error(env, 19);
        return Outcome::Return(0);
    }
    let from = w.vfs.lookup(&src).filter(|p| p.is_file());
    if from.is_none() && !optical {
        set_last_error(env, err::FILE_NOT_FOUND);
        return Outcome::Return(0);
    }
    if fail_if_exists && w.vfs.lookup(&dst).is_some() {
        set_last_error(env, err::FILE_EXISTS);
        return Outcome::Return(0);
    }
    let full = w.vfs.normalize(&dst);
    let parent = full.trim_end_matches('\\').rsplit_once('\\').map(|(p, _)| {
        if p.len() == 2 {
            format!("{p}\\")
        } else {
            p.to_owned()
        }
    });
    if !parent.is_some_and(|p| w.vfs.is_dir(&p)) {
        set_last_error(env, err::PATH_NOT_FOUND);
        return Outcome::Return(0);
    }
    if remove_src && w.vfs.is_base_only(&src) {
        set_last_error(env, err::ACCESS_DENIED);
        return Outcome::Return(0);
    }
    // Move the actual writable object; a failed removal must not report success.
    if remove_src {
        let Some(to) = w.vfs.write_path(&dst) else {
            set_last_error(env, err::PATH_NOT_FOUND);
            return Outcome::Return(0);
        };
        return match std::fs::rename(from.as_ref().unwrap(), &to) {
            Ok(()) => Outcome::Return(1),
            Err(e) => {
                io_error(env, &e);
                Outcome::Return(0)
            }
        };
    }
    // Resolve alias/hardlink identity before opening or writing the destination.
    // Normalizing names alone cannot detect two names for the same host file.
    let mut source = match if optical {
        w.vfs.open_optical_file(&src)
    } else {
        std::fs::File::open(from.as_ref().unwrap()).map(adv_os::vfs::FileStream::from)
    } {
        Ok(file) => file,
        Err(error) => {
            io_error(env, &error);
            return Outcome::Return(0);
        }
    };
    let identity = match source.cached_identity() {
        Some(identity) => identity,
        None => {
            set_last_error(env, err::ACCESS_DENIED);
            return Outcome::Return(0);
        }
    };
    if w.vfs
        .normalize(&src)
        .eq_ignore_ascii_case(&w.vfs.normalize(&dst))
        || w.vfs
            .lookup(&dst)
            .filter(|p| p.is_file())
            .is_some_and(|path| {
                std::fs::File::open(path)
                    .and_then(|file| adv_os::vfs::file_identity(&file))
                    .ok()
                    == Some(identity)
            })
    {
        set_last_error(env, 32); // ERROR_SHARING_VIOLATION, before copy-up/truncation.
        return Outcome::Return(0);
    }
    let content = if w.vfs.sections.has_entries() {
        w.vfs.sections.lookup(identity)
    } else {
        None
    };
    let length = match source.metadata() {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            io_error(env, &error);
            return Outcome::Return(0);
        }
    };
    let result = replace_contents(env, &dst, length, true, |destination, target| {
        let mut bytes = [0; 65536];
        let mut offset = 0;
        while offset < length {
            let count = (length - offset).min(bytes.len() as u64) as usize;
            source.read_exact(&mut bytes[..count])?;
            if let Some(content) = &content {
                content.merge_read(offset, &mut bytes[..count])?;
            }
            if let Some(target) = target {
                target.write_to(destination, offset, &bytes[..count])?;
            } else {
                destination.write_all(&bytes[..count])?;
            }
            offset += count as u64;
        }
        Ok(())
    });
    match result {
        Ok(()) => Outcome::Return(1),
        Err(code) => {
            set_last_error(env, code);
            Outcome::Return(0)
        }
    }
}

pub(super) fn create_directory(env: &mut Env<'_>, path: String) -> Outcome {
    match create_directory_path(env, &path) {
        Ok(()) => Outcome::Return(1),
        Err(code) => {
            set_last_error(env, code);
            Outcome::Return(0)
        }
    }
}

/// Kernel32 and CRT directory fronts share this checked, single-component VFS
/// mutation. write_path may mirror already-existing guest parents in an overlay;
/// it cannot make an absent guest parent valid for this operation.
pub(crate) fn create_directory_path(env: &Env<'_>, path: &str) -> Result<(), u32> {
    if path.is_empty() {
        return Err(err::PATH_NOT_FOUND);
    }
    let w = env.ext.get::<Win32>();
    if w.vfs.lookup(path).is_some() {
        return Err(err::ALREADY_EXISTS);
    }
    let full = w.vfs.normalize(path);
    let parent = full.trim_end_matches('\\').rsplit_once('\\').map(|(p, _)| {
        if p.len() == 2 {
            format!("{p}\\")
        } else {
            p.to_owned()
        }
    });
    if !parent.is_some_and(|p| w.vfs.is_dir(&p)) {
        return Err(err::PATH_NOT_FOUND);
    }
    let destination = w.vfs.write_path(path).ok_or(err::PATH_NOT_FOUND)?;
    std::fs::create_dir(destination).map_err(|error| match error.kind() {
        std::io::ErrorKind::AlreadyExists => err::ALREADY_EXISTS,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => err::PATH_NOT_FOUND,
        std::io::ErrorKind::PermissionDenied => err::ACCESS_DENIED,
        std::io::ErrorKind::InvalidInput => 123, // ERROR_INVALID_NAME
        std::io::ErrorKind::OutOfMemory => err::NOT_ENOUGH_MEMORY,
        std::io::ErrorKind::StorageFull => 112, // ERROR_DISK_FULL
        _ => io_code(&error),
    })
}

/// WIN32_FIND_DATAA/W: attrs, 3 FILETIMEs, size hi/lo, 2 reserved, name[260], alt[14]
/// (in bytes for A, UTF-16 units for W).
fn fill_find_data(env: &mut Env<'_>, p: u32, e: &DirEntry, wide: bool) {
    zero(env.cpu, p, if wide { 592 } else { 320 });
    let _ = env.cpu.write_u32(p, e.attributes);
    let ft = filetime(e.modified);
    for off in [4, 12, 20] {
        write_filetime(env.cpu, p + off, ft);
    }
    let _ = env.cpu.write_u32(p + 28, (e.size >> 32) as u32);
    let _ = env.cpu.write_u32(p + 32, e.size as u32);
    if wide {
        put_wstr(
            env.cpu,
            p + 44,
            260,
            &e.name.encode_utf16().collect::<Vec<_>>(),
        );
    } else {
        let name = env.ext.get::<Win32>().codec.encode(&e.name);
        put_cstr(env.cpu, p + 44, 260, &name);
    }
}

/// FindFirstFile{A,W} and FindFirstFileEx{A,W} (whose info level and search filters change
/// nothing here: the short name is always empty and every entry matches).
fn find_first_file(env: &mut Env<'_>, pat: String, data: u32, wide: bool) -> Outcome {
    let w = env.ext.get::<Win32>();
    let full = w.vfs.normalize(&pat);
    let (dir, mask) = full.rsplit_once('\\').unwrap_or(("", &full));
    let dir = if dir.len() == 2 {
        format!("{dir}\\")
    } else {
        dir.to_owned()
    };
    let entries: Vec<DirEntry> = w
        .vfs
        .list(&dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| wildcard(mask, &e.name))
        .collect();
    if entries.is_empty() {
        log::debug!("FindFirstFile({full}): no match");
        set_last_error(env, err::FILE_NOT_FOUND);
        return Outcome::Return(INVALID_HANDLE_VALUE);
    }
    fill_find_data(env, data, &entries[0], wide);
    let handle = env.sys.objects.insert_host(FindObj { entries, next: 1 });
    if handle == 0 {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
        return Outcome::Return(INVALID_HANDLE_VALUE);
    }
    Outcome::Return(handle)
}

fn find_next_file(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (h, data) = (env.arg(0), env.arg(1));
    let Some(f) = env.sys.objects.host_mut::<FindObj>(h) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    let Some(e) = f.entries.get(f.next).cloned() else {
        set_last_error(env, err::NO_MORE_FILES);
        return Outcome::Return(0);
    };
    f.next += 1;
    fill_find_data(env, data, &e, wide);
    Outcome::Return(1)
}

// ---- .ini profiles -------------------------------------------------------------------------

/// Guest path of a profile file: bare names live in the Windows directory.
fn profile_path(env: &Env<'_>, p: u32) -> String {
    let name = path_a(env, p);
    if name.contains(['\\', '/']) {
        name
    } else {
        format!("C:\\Windows\\{name}")
    }
}

struct Ini {
    lines: Vec<Vec<u8>>,
}

fn trim(b: &[u8]) -> &[u8] {
    let s = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let e = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(s, |i| i + 1);
    &b[s..e]
}

fn section_name(line: &[u8]) -> Option<&[u8]> {
    let t = trim(line);
    (t.first() == Some(&b'[')).then(|| {
        let end = t.iter().position(|&c| c == b']').unwrap_or(t.len());
        trim(&t[1..end])
    })
}

impl Ini {
    fn load(env: &Env<'_>, guest: &str) -> Ini {
        Self::from_bytes(&read_contents(env, guest).unwrap_or_default())
    }

    fn from_bytes(data: &[u8]) -> Ini {
        Ini {
            lines: data
                .split(|&c| c == b'\n')
                .map(|l| l.strip_suffix(b"\r").unwrap_or(l).to_vec())
                .collect(),
        }
    }

    /// Line range of a section's body.
    fn section(&self, name: &[u8]) -> Option<(usize, usize)> {
        let start = self
            .lines
            .iter()
            .position(|l| section_name(l).is_some_and(|s| s.eq_ignore_ascii_case(name)))?
            + 1;
        let end = self.lines[start..]
            .iter()
            .position(|l| section_name(l).is_some())
            .map_or(self.lines.len(), |i| start + i);
        Some((start, end))
    }

    fn entries(&self, section: &[u8]) -> Vec<(usize, Vec<u8>, Vec<u8>)> {
        let Some((s, e)) = self.section(section) else {
            return Vec::new();
        };
        (s..e)
            .filter_map(|i| {
                let l = &self.lines[i];
                let t = trim(l);
                if t.is_empty() || t[0] == b';' {
                    return None;
                }
                let eq = l.iter().position(|&c| c == b'=')?;
                let mut v = trim(&l[eq + 1..]).to_vec();
                if v.len() >= 2 && (v[0] == b'"' && v[v.len() - 1] == b'"') {
                    v = v[1..v.len() - 1].to_vec();
                }
                Some((i, trim(&l[..eq]).to_vec(), v))
            })
            .collect()
    }

    fn get(&self, section: &[u8], key: &[u8]) -> Option<Vec<u8>> {
        self.entries(section)
            .into_iter()
            .find(|(_, k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, _, v)| v)
    }
}

fn get_private_profile_int_a(env: &mut Env<'_>) -> Outcome {
    let (sec, key, def) = (
        cstr(env.cpu, env.arg(0)),
        cstr(env.cpu, env.arg(1)),
        env.arg(2),
    );
    let file = profile_path(env, env.arg(3));
    let Some(v) = Ini::load(env, &file).get(&sec, &key) else {
        return Outcome::Return(def);
    };
    let s = String::from_utf8_lossy(&v);
    let s = s.trim();
    let (neg, digits) = s.strip_prefix('-').map_or((false, s), |r| (true, r));
    let n: String = digits.chars().take_while(char::is_ascii_digit).collect();
    let v: i64 = n.parse().unwrap_or(0);
    Outcome::Return((if neg { -v } else { v }) as i32 as u32)
}

fn write_private_profile_string_a(env: &mut Env<'_>) -> Outcome {
    let (sec_p, key_p, val_p) = (env.arg(0), env.arg(1), env.arg(2));
    let file = profile_path(env, env.arg(3));
    let mut ini = match read_contents(env, &file) {
        Ok(bytes) if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) => {
            adv_abi::warn_unavailable(
                env,
                "kernel32.dll!WritePrivateProfileStringA",
                "Unicode profile-file writes are unavailable; preserving the existing encoded file",
            );
            set_last_error(env, 50);
            return Outcome::Return(0);
        }
        Ok(bytes) => Ini::from_bytes(&bytes),
        Err(error) => {
            io_error(env, &error);
            return Outcome::Return(0);
        }
    };
    if ini.lines.last().is_some_and(Vec::is_empty) {
        ini.lines.pop();
    }
    let sec = cstr(env.cpu, sec_p);
    match ini.section(&sec) {
        None if key_p == 0 || val_p == 0 => {}
        None => {
            ini.lines.push([b"[", &sec[..], b"]"].concat());
            ini.lines
                .push([&cstr(env.cpu, key_p)[..], b"=", &cstr(env.cpu, val_p)[..]].concat());
        }
        Some((s, e)) if key_p == 0 => {
            ini.lines.drain(s - 1..e);
        }
        Some((s, e)) => {
            let key = cstr(env.cpu, key_p);
            let existing = ini
                .entries(&sec)
                .into_iter()
                .find(|(_, k, _)| k.eq_ignore_ascii_case(&key))
                .map(|(i, _, _)| i);
            match (existing, val_p) {
                (Some(i), 0) => {
                    ini.lines.remove(i);
                }
                (Some(i), v) => ini.lines[i] = [&key[..], b"=", &cstr(env.cpu, v)[..]].concat(),
                (None, 0) => {}
                (None, v) => {
                    // Append after the section's last non-empty line.
                    let at = (s..e)
                        .rev()
                        .find(|&i| !trim(&ini.lines[i]).is_empty())
                        .map_or(s, |i| i + 1);
                    ini.lines
                        .insert(at, [&key[..], b"=", &cstr(env.cpu, v)[..]].concat());
                }
            }
        }
    }
    let mut out = ini.lines.join(&b"\r\n"[..]);
    out.extend_from_slice(b"\r\n");
    match replace_contents(
        env,
        &file,
        out.len() as u64,
        false,
        |destination, content| {
            if let Some(content) = content {
                content.write_to(destination, 0, &out)
            } else {
                destination.write_all(&out)
            }
        },
    ) {
        Ok(()) => Outcome::Return(1),
        Err(code) => {
            set_last_error(env, code);
            Outcome::Return(0)
        }
    }
}

/// Fixed guest system volume plus explicitly attached optical media.
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_FIXED: u32 = 3;

/// Drive letter a root path names (the current drive for NULL / relative paths).
fn drive_of(env: &Env<'_>, root: u32) -> Option<u8> {
    let p = if root == 0 {
        Vec::new()
    } else {
        cstr(env.cpu, root)
    };
    let d = match p.get(1) {
        Some(b':') => p[0],
        _ => env.ext.get::<Win32>().vfs.cwd.as_bytes()[0],
    };
    Some(d.to_ascii_uppercase()).filter(|d| d.is_ascii_uppercase())
}

fn get_drive_type(env: &mut Env<'_>) -> Outcome {
    let d = drive_of(env, env.arg(0));
    Outcome::Return(if d == Some(b'C') {
        DRIVE_FIXED
    } else if d.is_some_and(|d| {
        env.ext
            .get::<Win32>()
            .vfs
            .optical_drive(d as char)
            .is_some()
    }) {
        5 // DRIVE_CDROM
    } else {
        DRIVE_NO_ROOT_DIR
    })
}

fn drive_of_w(env: &Env<'_>, root: u32) -> Option<u8> {
    if root == 0 {
        return Some(env.ext.get::<Win32>().vfs.cwd.as_bytes()[0].to_ascii_uppercase());
    }
    let p = wstr(env.cpu, root);
    let d = match p.get(1) {
        Some(&0x3a) => p[0] as u8,
        _ => env.ext.get::<Win32>().vfs.cwd.as_bytes()[0],
    };
    Some(d.to_ascii_uppercase()).filter(|d| d.is_ascii_uppercase())
}

fn get_drive_type_w(env: &mut Env<'_>) -> Outcome {
    let d = drive_of_w(env, env.arg(0));
    Outcome::Return(if d == Some(b'C') {
        DRIVE_FIXED
    } else if d.is_some_and(|d| {
        env.ext
            .get::<Win32>()
            .vfs
            .optical_drive(d as char)
            .is_some()
    }) {
        5
    } else {
        DRIVE_NO_ROOT_DIR
    })
}

fn query_dos_device(
    env: &mut Env<'_>,
    name: Option<String>,
    target: u32,
    cap: u32,
    wide: bool,
) -> Outcome {
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    if cap == 0 || target == 0 {
        set_last_error(env, ERROR_INSUFFICIENT_BUFFER);
        return Outcome::Return(0);
    }
    let mapping = match name {
        None => drive_letters(env)
            .into_iter()
            .map(|d| format!("{d}:"))
            .collect(),
        Some(s) => {
            let trimmed = s.trim_end_matches('\\');
            if trimmed.eq_ignore_ascii_case("C:") || trimmed.eq_ignore_ascii_case("C") {
                vec![r"\Device\HarddiskVolume1".to_string()]
            } else if let Some((index, _)) = env
                .ext
                .get::<Win32>()
                .vfs
                .optical_drives()
                .enumerate()
                .find(|(_, (letter, _))| trimmed.eq_ignore_ascii_case(&format!("{letter}:")))
            {
                vec![format!(r"\Device\CdRom{index}")]
            } else {
                set_last_error(env, ERROR_FILE_NOT_FOUND);
                return Outcome::Return(0);
            }
        }
    };
    if wide {
        let mut units = Vec::new();
        for item in &mapping {
            units.extend(item.encode_utf16());
            units.push(0);
        }
        units.push(0);
        if cap < units.len() as u32 {
            set_last_error(env, ERROR_INSUFFICIENT_BUFFER);
            return Outcome::Return(0);
        }
        crate::util::write_units(env.cpu, target, &units);
        Outcome::Return(units.len() as u32)
    } else {
        let mut bytes = Vec::new();
        for item in &mapping {
            bytes.extend_from_slice(item.as_bytes());
            bytes.push(0);
        }
        bytes.push(0);
        if cap < bytes.len() as u32 {
            set_last_error(env, ERROR_INSUFFICIENT_BUFFER);
            return Outcome::Return(0);
        }
        let _ = env.cpu.write(target, &bytes);
        Outcome::Return(bytes.len() as u32)
    }
}

fn query_dos_device_a(env: &mut Env<'_>) -> Outcome {
    let (name_p, target, cap) = (env.arg(0), env.arg(1), env.arg(2));
    let name = if name_p == 0 {
        None
    } else {
        Some(String::from_utf8_lossy(&cstr(env.cpu, name_p)).into_owned())
    };
    query_dos_device(env, name, target, cap, false)
}

fn query_dos_device_w(env: &mut Env<'_>) -> Outcome {
    let (name_p, target, cap) = (env.arg(0), env.arg(1), env.arg(2));
    let name = if name_p == 0 {
        None
    } else {
        Some(String::from_utf16_lossy(&wstr(env.cpu, name_p)))
    };
    query_dos_device(env, name, target, cap, true)
}

/// GetVolumeInformationA(root, name, name_cap, serial, max_component, flags, fs, fs_cap).
fn get_volume_information(env: &mut Env<'_>, wide: bool) -> Outcome {
    let drive = if wide {
        drive_of_w(env, env.arg(0))
    } else {
        drive_of(env, env.arg(0))
    };
    let optical = drive.and_then(|d| env.ext.get::<Win32>().vfs.optical_drive(d as char));
    let (label, id, component, attributes, filesystem) = if let Some(disc) = optical {
        (
            disc.label.clone(),
            disc.serial,
            disc.max_component_length,
            disc.filesystem_flags,
            disc.filesystem.to_string(),
        )
    } else if drive == Some(b'C') {
        (String::new(), 0x5041_4c00, 255, 0xf, "NTFS".into())
    } else {
        set_last_error(env, err::PATH_NOT_FOUND);
        return Outcome::Return(0);
    };
    let (name, name_cap, fs, fs_cap) = (env.arg(1), env.arg(2), env.arg(6), env.arg(7));
    let (serial, max_component, flags) = (env.arg(3), env.arg(4), env.arg(5));
    let encode = |s: &str| -> Vec<u8> {
        if wide {
            s.encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect()
        } else {
            let mut b = env.ext.get::<Win32>().codec.encode(s);
            b.push(0);
            b
        }
    };
    let label = encode(&label);
    let filesystem = encode(&filesystem);
    for (pointer, cap, bytes) in [(name, name_cap, &label), (fs, fs_cap, &filesystem)] {
        if pointer == 0 {
            continue;
        }
        if u64::from(cap) * if wide { 2 } else { 1 } < bytes.len() as u64 {
            set_last_error(env, 234); // ERROR_MORE_DATA
            return Outcome::Return(0);
        }
        if adv_abi::guest::check_range(
            env.cpu,
            pointer,
            bytes.len() as u32,
            adv_cpu::backend::Prot::WRITE,
        )
        .is_err()
        {
            set_last_error(env, 998);
            return Outcome::Return(0);
        }
    }
    for pointer in [serial, max_component, flags] {
        if pointer != 0
            && adv_abi::guest::check_range(env.cpu, pointer, 4, adv_cpu::backend::Prot::WRITE)
                .is_err()
        {
            set_last_error(env, 998);
            return Outcome::Return(0);
        }
    }
    if name != 0 {
        let _ = env.cpu.write(name, &label);
    }
    if fs != 0 {
        let _ = env.cpu.write(fs, &filesystem);
    }
    out_u32(env.cpu, serial, id);
    out_u32(env.cpu, max_component, component);
    out_u32(env.cpu, flags, attributes);
    Outcome::Return(1)
}

fn get_disk_free_space(env: &mut Env<'_>, wide: bool) -> Outcome {
    let root = env.arg(0);
    let drive = if wide {
        drive_of_w(env, root)
    } else {
        drive_of(env, root)
    };
    let geometry = if let Some(disc) =
        drive.and_then(|d| env.ext.get::<Win32>().vfs.optical_drive(d as char))
    {
        (
            1,
            disc.bytes_per_sector,
            0,
            (disc.total_bytes / u64::from(disc.bytes_per_sector)) as u32,
        )
    } else if drive == Some(b'C') {
        (8, 512, 500_000, 524_288)
    } else {
        set_last_error(env, err::PATH_NOT_FOUND);
        return Outcome::Return(0);
    };
    let (sectors, bytes, free, total) = (env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    out_u32(env.cpu, sectors, geometry.0);
    out_u32(env.cpu, bytes, geometry.1);
    out_u32(env.cpu, free, geometry.2);
    out_u32(env.cpu, total, geometry.3);
    Outcome::Return(1)
}

/// GetDiskFreeSpaceEx: a fixed, roomy `C:` (64 GiB free of 256 GiB) so save-space checks
/// pass the same way on every host.
fn get_disk_free_space_ex(env: &mut Env<'_>, wide: bool) -> Outcome {
    const FREE: u64 = 64 << 30;
    const TOTAL: u64 = 256 << 30;
    let root = env.arg(0);
    let drive = if wide {
        drive_of_w(env, root)
    } else {
        drive_of(env, root)
    };
    let (free, total) = if let Some(disc) =
        drive.and_then(|d| env.ext.get::<Win32>().vfs.optical_drive(d as char))
    {
        (0, disc.total_bytes)
    } else if drive == Some(b'C') {
        (FREE, TOTAL)
    } else {
        set_last_error(env, err::PATH_NOT_FOUND);
        return Outcome::Return(0);
    };
    for (i, v) in [(1, free), (2, total), (3, free)] {
        let p = env.arg(i);
        if p != 0 {
            out_u32(env.cpu, p, v as u32);
            out_u32(env.cpu, p + 4, (v >> 32) as u32);
        }
    }
    Outcome::Return(1)
}

fn drive_letters(env: &Env<'_>) -> Vec<char> {
    let mut drives = vec!['C'];
    drives.extend(
        env.ext
            .get::<Win32>()
            .vfs
            .optical_drives()
            .map(|(letter, _)| letter),
    );
    drives.sort_unstable();
    drives
}
fn get_logical_drive_strings(env: &mut Env<'_>, cap: u32, buf: u32, wide: bool) -> Outcome {
    let drives = drive_letters(env);
    let required = drives.len() as u32 * 4 + 1;
    if cap < required || buf == 0 {
        return Outcome::Return(required);
    }
    let mut bytes: Vec<u8> = drives
        .iter()
        .flat_map(|d| [*d as u8, b':', b'\\', 0])
        .collect();
    bytes.push(0);
    if wide {
        let units: Vec<u16> = bytes.into_iter().map(u16::from).collect();
        crate::util::write_units(env.cpu, buf, &units);
    } else {
        let _ = env.cpu.write(buf, &bytes);
    }
    Outcome::Return(required - 1)
}

fn get_logical_drive_strings_a(env: &mut Env<'_>) -> Outcome {
    let (cap, buf) = (env.arg(0), env.arg(1));
    get_logical_drive_strings(env, cap, buf, false)
}

fn get_logical_drive_strings_w(env: &mut Env<'_>) -> Outcome {
    let (cap, buf) = (env.arg(0), env.arg(1));
    get_logical_drive_strings(env, cap, buf, true)
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "GetLogicalDrives", 0, |env| {
        Outcome::Return(
            drive_letters(env)
                .iter()
                .fold(0, |mask, d| mask | (1 << (*d as u32 - 'A' as u32))),
        )
    });
    stdcall(
        reg,
        K32,
        "GetLogicalDriveStringsA",
        2,
        get_logical_drive_strings_a,
    );
    stdcall(
        reg,
        K32,
        "GetLogicalDriveStringsW",
        2,
        get_logical_drive_strings_w,
    );
    stdcall(reg, K32, "GetDriveTypeA", 1, get_drive_type);
    stdcall(reg, K32, "GetDriveTypeW", 1, get_drive_type_w);
    stdcall(reg, K32, "QueryDosDeviceA", 3, query_dos_device_a);
    stdcall(reg, K32, "QueryDosDeviceW", 3, query_dos_device_w);
    stdcall(reg, K32, "GetVolumeInformationA", 8, |env| {
        get_volume_information(env, false)
    });
    stdcall(reg, K32, "GetVolumeInformationW", 8, |env| {
        get_volume_information(env, true)
    });
    stdcall(reg, K32, "AreFileApisANSI", 0, |_| Outcome::Return(1));
    stdcall(reg, K32, "SetFileApisToOEM", 0, |_| Outcome::Return(0));
    stdcall(reg, K32, "SetFileApisToANSI", 0, |_| Outcome::Return(0));
    stdcall(reg, K32, "GetDiskFreeSpaceA", 5, |e| {
        get_disk_free_space(e, false)
    });
    stdcall(reg, K32, "GetDiskFreeSpaceW", 5, |e| {
        get_disk_free_space(e, true)
    });
    stdcall(reg, K32, "GetDiskFreeSpaceExA", 4, |e| {
        get_disk_free_space_ex(e, false)
    });
    stdcall(reg, K32, "GetDiskFreeSpaceExW", 4, |e| {
        get_disk_free_space_ex(e, true)
    });
    stdcall(reg, K32, "CreateFileA", 7, |e| {
        let p = path_a(e, e.arg(0));
        create_file(e, p)
    });
    stdcall(reg, K32, "CreateFileW", 7, |e| {
        let p = path_w(e, e.arg(0));
        create_file(e, p)
    });
    stdcall(reg, K32, "ReadFile", 5, read_file);
    stdcall(reg, K32, "WriteFile", 5, write_file);
    stdcall(reg, K32, "SetFilePointer", 4, set_file_pointer);
    stdcall(reg, K32, "SetFilePointerEx", 5, set_file_pointer_ex);
    stdcall(reg, K32, "GetFileSize", 2, get_file_size);
    stdcall(reg, K32, "GetFileSizeEx", 2, get_file_size_ex);
    stdcall(reg, K32, "GetFileType", 1, get_file_type);
    stdcall(reg, K32, "FlushFileBuffers", 1, |_| Outcome::Return(1));
    stdcall(reg, K32, "SetEndOfFile", 1, set_end_of_file);
    stdcall(reg, K32, "GetFileTime", 4, get_file_time);
    stdcall(reg, K32, "SetFileTime", 4, |_| Outcome::Return(1));
    attributes::register(reg);
    stdcall(reg, K32, "FindFirstFileA", 2, |e| {
        let p = path_a(e, e.arg(0));
        find_first_file(e, p, e.arg(1), false)
    });
    stdcall(reg, K32, "FindFirstFileW", 2, |e| {
        let p = path_w(e, e.arg(0));
        find_first_file(e, p, e.arg(1), true)
    });
    stdcall(reg, K32, "FindFirstFileExA", 6, |e| {
        let p = path_a(e, e.arg(0));
        find_first_file(e, p, e.arg(2), false)
    });
    stdcall(reg, K32, "FindFirstFileExW", 6, |e| {
        let p = path_w(e, e.arg(0));
        find_first_file(e, p, e.arg(2), true)
    });
    stdcall(reg, K32, "FindNextFileA", 2, |e| find_next_file(e, false));
    stdcall(reg, K32, "FindNextFileW", 2, |e| find_next_file(e, true));
    stdcall(reg, K32, "FindClose", 1, |e| {
        let h = e.arg(0);
        let find = e.sys.objects.host::<FindObj>(h).is_some();
        if find {
            e.sys.objects.close(h);
        }
        Outcome::Return(u32::from(find))
    });
    path::register(reg);
    profile::register(reg);

    stdcall(
        reg,
        K32,
        "GetPrivateProfileIntA",
        4,
        get_private_profile_int_a,
    );
    stdcall(
        reg,
        K32,
        "WritePrivateProfileStringA",
        4,
        write_private_profile_string_a,
    );
    stdcall(reg, K32, "WriteConsoleW", 5, |e| {
        let (buf, n, written) = (e.arg(1), e.arg(2), e.arg(3));
        let u = crate::util::units_or_wstr(e.cpu, buf, n as i32);
        log::info!(
            "[guest console] {}",
            String::from_utf16_lossy(&u).trim_end()
        );
        out_u32(e.cpu, written, n);
        Outcome::Return(1)
    });
}
