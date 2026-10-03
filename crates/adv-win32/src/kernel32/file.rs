//! Files, directories, and .ini profiles, all through the [`crate::vfs::Vfs`].

use std::io::{Read, Seek, SeekFrom, Write};
use std::time::SystemTime;

use adv_machine::{Env, Object, Outcome, Registry};
use pal_cpu::{Guest, GuestExt};

use super::K32;
use crate::util::{
    INVALID_HANDLE_VALUE, cstr, err, out_u32, put_cstr, put_wstr, set_last_error, wstr, zero,
};
use crate::vfs::{DirEntry, wildcard};
use crate::{FileObj, FindObj, STD_ERROR, STD_INPUT, STD_OUTPUT, Win32, stdcall};

const GENERIC_WRITE: u32 = 0x4000_0000;
const CREATE_NEW: u32 = 1;
const CREATE_ALWAYS: u32 = 2;
const OPEN_EXISTING: u32 = 3;
const OPEN_ALWAYS: u32 = 4;
const TRUNCATE_EXISTING: u32 = 5;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;

fn path_a(env: &Env<'_>, p: u32) -> String {
    env.ext.get::<Win32>().codec.decode(&cstr(env.cpu, p))
}

fn path_w(env: &Env<'_>, p: u32) -> String {
    String::from_utf16_lossy(&wstr(env.cpu, p))
}

fn io_error(env: &mut Env<'_>, e: &std::io::Error) {
    let code = match e.kind() {
        std::io::ErrorKind::NotFound => err::FILE_NOT_FOUND,
        std::io::ErrorKind::PermissionDenied => err::ACCESS_DENIED,
        std::io::ErrorKind::AlreadyExists => err::FILE_EXISTS,
        _ => err::ACCESS_DENIED,
    };
    set_last_error(env, code);
}

fn create_file(env: &mut Env<'_>, path: String) -> Outcome {
    let (access, disposition) = (env.arg(1), env.arg(4));
    Outcome::Return(open(env, &path, access, disposition))
}

/// CreateFile's core for host callers (PAL's file API): open `path` with the given access
/// and creation disposition, register the handle, and set the last error. Returns the
/// handle or `INVALID_HANDLE_VALUE`.
pub fn open(env: &mut Env<'_>, path: &str, access: u32, disposition: u32) -> u32 {
    let w = env.ext.get::<Win32>();
    let guest = w.vfs.normalize(path);
    let existing = w.vfs.lookup(&guest);
    let write = access & GENERIC_WRITE != 0
        || matches!(disposition, CREATE_NEW | CREATE_ALWAYS | TRUNCATE_EXISTING);
    let exists = existing.as_ref().is_some_and(|p| p.is_file());

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
                    if let Err(e) = std::fs::copy(src, &dst) {
                        io_error(env, &e);
                        return INVALID_HANDLE_VALUE;
                    }
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
            let h = env.sys.objects.insert(Object::Opaque(1));
            log::debug!("CreateFile({guest}, {access:#x}, {disposition}) = {h:#x}");
            // ERROR_ALREADY_EXISTS is reported for CREATE_ALWAYS / OPEN_ALWAYS on success.
            let code = if exists && matches!(disposition, CREATE_ALWAYS | OPEN_ALWAYS) {
                err::ALREADY_EXISTS
            } else {
                0
            };
            set_last_error(env, code);
            env.ext.get_mut::<Win32>().files.insert(
                h,
                FileObj {
                    file,
                    guest_path: guest,
                    access,
                },
            );
            h
        }
        Err(e) => {
            log::debug!("CreateFile({guest}) failed: {e}");
            io_error(env, &e);
            INVALID_HANDLE_VALUE
        }
    }
}

fn read_file(env: &mut Env<'_>) -> Outcome {
    let (h, buf, n, read_out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let Some(f) = env.ext.get_mut::<Win32>().files.get_mut(&h) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    let mut data = vec![0u8; n as usize];
    let mut got = 0;
    while got < data.len() {
        match f.file.read(&mut data[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(e) => {
                io_error(env, &e);
                return Outcome::Return(0);
            }
        }
    }
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
    let Some(f) = env.ext.get_mut::<Win32>().files.get_mut(&h) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    match f.file.write_all(&data) {
        Ok(()) => {
            out_u32(env.cpu, written, n);
            Outcome::Return(1)
        }
        Err(e) => {
            io_error(env, &e);
            Outcome::Return(0)
        }
    }
}

/// Move `h`'s file pointer; the new position, or `None` with the last error set.
fn seek(env: &mut Env<'_>, h: u32, dist: i64, method: u32) -> Option<u64> {
    let Some(f) = env.ext.get_mut::<Win32>().files.get_mut(&h) else {
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
    env.ext
        .get::<Win32>()
        .files
        .get(&h)
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
        _ if env.ext.get::<Win32>().files.contains_key(&h) => 1,
        _ => 0,
    })
}

fn close_handle(env: &mut Env<'_>) -> Outcome {
    let h = env.arg(0);
    let object = env.sys.objects.remove(h).is_some();
    super::file_mapping::close(env, h);
    let w = env.ext.get_mut::<Win32>();
    let known = w.files.remove(&h).is_some()
        | w.finds.remove(&h).is_some()
        | w.thread_snapshots.remove(&h).is_some();
    if object || known || matches!(h, STD_INPUT | STD_OUTPUT | STD_ERROR) || h >= 0xffff_fffe {
        return Outcome::Return(1);
    }
    log::debug!("CloseHandle({h:#x}): unknown handle");
    set_last_error(env, err::INVALID_HANDLE);
    Outcome::Return(0)
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
        .ext
        .get::<Win32>()
        .files
        .get(&h)
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

fn attributes(is_dir: bool) -> u32 {
    if is_dir {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_ARCHIVE
    }
}

fn get_file_attributes(env: &mut Env<'_>, path: String) -> Outcome {
    match env.ext.get::<Win32>().vfs.lookup(&path) {
        Some(p) => Outcome::Return(attributes(p.is_dir())),
        None => {
            set_last_error(env, err::FILE_NOT_FOUND);
            Outcome::Return(u32::MAX)
        }
    }
}

fn delete_file(env: &mut Env<'_>, path: String) -> Outcome {
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

fn copy_file(
    env: &mut Env<'_>,
    src: String,
    dst: String,
    fail_if_exists: bool,
    remove_src: bool,
) -> Outcome {
    let w = env.ext.get::<Win32>();
    let Some(from) = w.vfs.lookup(&src).filter(|p| p.is_file()) else {
        set_last_error(env, err::FILE_NOT_FOUND);
        return Outcome::Return(0);
    };
    if fail_if_exists && w.vfs.lookup(&dst).is_some() {
        set_last_error(env, err::FILE_EXISTS);
        return Outcome::Return(0);
    }
    let Some(to) = w.vfs.write_path(&dst) else {
        set_last_error(env, err::PATH_NOT_FOUND);
        return Outcome::Return(0);
    };
    let base_only = w.vfs.is_base_only(&src);
    if let Err(e) = std::fs::copy(&from, &to) {
        io_error(env, &e);
        return Outcome::Return(0);
    }
    if remove_src && !base_only {
        let _ = std::fs::remove_file(from);
    }
    Outcome::Return(1)
}

fn create_directory(env: &mut Env<'_>, path: String) -> Outcome {
    let w = env.ext.get::<Win32>();
    if w.vfs.lookup(&path).is_some() {
        set_last_error(env, err::ALREADY_EXISTS);
        return Outcome::Return(0);
    }
    match w.vfs.write_path(&path).map(std::fs::create_dir) {
        Some(Ok(())) => Outcome::Return(1),
        _ => {
            set_last_error(env, err::PATH_NOT_FOUND);
            Outcome::Return(0)
        }
    }
}

/// WIN32_FIND_DATAA/W: attrs, 3 FILETIMEs, size hi/lo, 2 reserved, name[260], alt[14]
/// (in bytes for A, UTF-16 units for W).
fn fill_find_data(env: &mut Env<'_>, p: u32, e: &DirEntry, wide: bool) {
    zero(env.cpu, p, if wide { 592 } else { 320 });
    let _ = env.cpu.write_u32(p, attributes(e.is_dir));
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
    let h = env.sys.objects.insert(Object::Opaque(2));
    env.ext
        .get_mut::<Win32>()
        .finds
        .insert(h, FindObj { entries, next: 1 });
    Outcome::Return(h)
}

fn find_next_file(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (h, data) = (env.arg(0), env.arg(1));
    let Some(f) = env.ext.get_mut::<Win32>().finds.get_mut(&h) else {
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

fn get_current_directory_a(env: &mut Env<'_>) -> Outcome {
    let (cap, buf) = (env.arg(0), env.arg(1));
    let w = env.ext.get::<Win32>();
    let b = w.codec.encode(&w.vfs.cwd);
    match put_cstr(env.cpu, buf, cap, &b) {
        Some(n) => Outcome::Return(n),
        None => Outcome::Return(b.len() as u32 + 1),
    }
}

fn set_current_directory(env: &mut Env<'_>, path: String) -> Outcome {
    let w = env.ext.get_mut::<Win32>();
    let n = w.vfs.normalize(&path);
    if !w.vfs.is_dir(&n) {
        set_last_error(env, err::PATH_NOT_FOUND);
        return Outcome::Return(0);
    }
    w.vfs.cwd = n;
    Outcome::Return(1)
}

fn get_full_path_name_a(env: &mut Env<'_>) -> Outcome {
    let (path, cap, buf, file_part) = (path_a(env, env.arg(0)), env.arg(1), env.arg(2), env.arg(3));
    let w = env.ext.get::<Win32>();
    let full = w.vfs.normalize(&path);
    let b = w.codec.encode(&full);
    match put_cstr(env.cpu, buf, cap, &b) {
        Some(n) => {
            let last = b.iter().rposition(|&c| c == b'\\').map_or(0, |i| i + 1) as u32;
            out_u32(env.cpu, file_part, buf + last);
            Outcome::Return(n)
        }
        None => Outcome::Return(b.len() as u32 + 1),
    }
}

fn put_dir(env: &mut Env<'_>, s: &str) -> Outcome {
    let (buf, cap) = (env.arg(0), env.arg(1));
    match put_cstr(env.cpu, buf, cap, s.as_bytes()) {
        Some(n) => Outcome::Return(n),
        None => Outcome::Return(s.len() as u32 + 1),
    }
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
        let data = env
            .ext
            .get::<Win32>()
            .vfs
            .lookup(guest)
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_default();
        Ini {
            lines: data
                .split(|&c| c == b'\n')
                .map(|l| l.strip_suffix(b"\r").unwrap_or(l).to_vec())
                .collect(),
        }
    }

    fn sections(&self) -> Vec<Vec<u8>> {
        self.lines
            .iter()
            .filter_map(|l| section_name(l))
            .map(<[u8]>::to_vec)
            .collect()
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

/// Copy a double-NUL-terminated list into `buf`, truncating like the API does.
fn put_list(env: &mut Env<'_>, buf: u32, cap: u32, items: &[Vec<u8>]) -> u32 {
    if cap < 2 {
        return 0;
    }
    let mut out = Vec::new();
    for it in items {
        out.extend_from_slice(it);
        out.push(0);
    }
    if out.len() + 1 > cap as usize {
        out.truncate(cap as usize - 2);
        out.extend_from_slice(&[0, 0]);
        let _ = env.cpu.write(buf, &out);
        return cap - 2;
    }
    out.push(0);
    let _ = env.cpu.write(buf, &out);
    out.len() as u32 - 1
}

fn get_private_profile_string_a(env: &mut Env<'_>) -> Outcome {
    let (sec_p, key_p, def_p, buf, cap) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let file = profile_path(env, env.arg(5));
    let ini = Ini::load(env, &file);
    if sec_p == 0 {
        let secs = ini.sections();
        return Outcome::Return(put_list(env, buf, cap, &secs));
    }
    let sec = cstr(env.cpu, sec_p);
    if key_p == 0 {
        let keys: Vec<Vec<u8>> = ini.entries(&sec).into_iter().map(|(_, k, _)| k).collect();
        return Outcome::Return(put_list(env, buf, cap, &keys));
    }
    let key = cstr(env.cpu, key_p);
    let v = ini
        .get(&sec, &key)
        .unwrap_or_else(|| trim(&cstr(env.cpu, def_p)).to_vec());
    Outcome::Return(match put_cstr(env.cpu, buf, cap, &v) {
        Some(n) => n,
        None => cap.saturating_sub(1),
    })
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
    let mut ini = Ini::load(env, &file);
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
    let w = env.ext.get::<Win32>();
    match w.vfs.write_path(&file).map(|p| std::fs::write(p, out)) {
        Some(Ok(())) => Outcome::Return(1),
        _ => {
            set_last_error(env, err::ACCESS_DENIED);
            Outcome::Return(0)
        }
    }
}

/// Drives: the guest has only a fixed `C:` (no removable or optical drives).
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
    } else {
        DRIVE_NO_ROOT_DIR
    })
}

/// GetVolumeInformationA(root, name, name_cap, serial, max_component, flags, fs, fs_cap).
fn get_volume_information(env: &mut Env<'_>) -> Outcome {
    if drive_of(env, env.arg(0)) != Some(b'C') {
        set_last_error(env, err::PATH_NOT_FOUND);
        return Outcome::Return(0);
    }
    let (name, name_cap, fs, fs_cap) = (env.arg(1), env.arg(2), env.arg(6), env.arg(7));
    if name != 0 {
        let _ = put_cstr(env.cpu, name, name_cap, b"");
    }
    let (serial, max_component, flags) = (env.arg(3), env.arg(4), env.arg(5));
    out_u32(env.cpu, serial, 0x5041_4c00);
    out_u32(env.cpu, max_component, 255);
    // FILE_CASE_SENSITIVE_SEARCH | CASE_PRESERVED_NAMES | UNICODE_ON_DISK | PERSISTENT_ACLS
    out_u32(env.cpu, flags, 0xf);
    if fs != 0 {
        let _ = put_cstr(env.cpu, fs, fs_cap, b"NTFS");
    }
    Outcome::Return(1)
}

/// GetDiskFreeSpaceExA(dir, free_to_caller*, total*, total_free*): a fixed, roomy `C:`
/// (64 GiB free of 256 GiB) so save-space checks pass the same way on every host.
fn get_disk_free_space_ex(env: &mut Env<'_>) -> Outcome {
    const FREE: u64 = 64 << 30;
    const TOTAL: u64 = 256 << 30;
    if drive_of(env, env.arg(0)) != Some(b'C') {
        set_last_error(env, err::PATH_NOT_FOUND);
        return Outcome::Return(0);
    }
    for (i, v) in [(1, FREE), (2, TOTAL), (3, FREE)] {
        let p = env.arg(i);
        if p != 0 {
            out_u32(env.cpu, p, v as u32);
            out_u32(env.cpu, p + 4, (v >> 32) as u32);
        }
    }
    Outcome::Return(1)
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "GetLogicalDrives", 0, |_| Outcome::Return(1 << 2));
    stdcall(reg, K32, "GetDriveTypeA", 1, get_drive_type);
    stdcall(reg, K32, "GetVolumeInformationA", 8, get_volume_information);
    stdcall(reg, K32, "GetDiskFreeSpaceExA", 4, get_disk_free_space_ex);
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
    stdcall(reg, K32, "SetEndOfFile", 1, |e| {
        let h = e.arg(0);
        let ok = e.ext.get_mut::<Win32>().files.get_mut(&h).is_some_and(|f| {
            let pos = f.file.stream_position().unwrap_or(0);
            f.file.set_len(pos).is_ok()
        });
        Outcome::Return(u32::from(ok))
    });
    stdcall(reg, K32, "CloseHandle", 1, close_handle);
    stdcall(reg, K32, "GetFileTime", 4, get_file_time);
    stdcall(reg, K32, "SetFileTime", 4, |_| Outcome::Return(1));
    stdcall(reg, K32, "GetFileAttributesA", 1, |e| {
        let p = path_a(e, e.arg(0));
        get_file_attributes(e, p)
    });
    stdcall(reg, K32, "GetFileAttributesW", 1, |e| {
        let p = path_w(e, e.arg(0));
        get_file_attributes(e, p)
    });
    stdcall(reg, K32, "SetFileAttributesA", 2, |_| Outcome::Return(1));
    stdcall(reg, K32, "DeleteFileA", 1, |e| {
        let p = path_a(e, e.arg(0));
        delete_file(e, p)
    });
    stdcall(reg, K32, "CopyFileA", 3, |e| {
        let (s, d, f) = (path_a(e, e.arg(0)), path_a(e, e.arg(1)), e.arg(2) != 0);
        copy_file(e, s, d, f, false)
    });
    stdcall(reg, K32, "MoveFileA", 2, |e| {
        let (s, d) = (path_a(e, e.arg(0)), path_a(e, e.arg(1)));
        copy_file(e, s, d, true, true)
    });
    stdcall(reg, K32, "CreateDirectoryA", 2, |e| {
        let p = path_a(e, e.arg(0));
        create_directory(e, p)
    });
    stdcall(reg, K32, "CreateDirectoryW", 2, |e| {
        let p = path_w(e, e.arg(0));
        create_directory(e, p)
    });
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
        e.sys.objects.remove(h);
        Outcome::Return(u32::from(
            e.ext.get_mut::<Win32>().finds.remove(&h).is_some(),
        ))
    });
    stdcall(reg, K32, "GetCurrentDirectoryA", 2, get_current_directory_a);
    stdcall(reg, K32, "SetCurrentDirectoryA", 1, |e| {
        let p = path_a(e, e.arg(0));
        set_current_directory(e, p)
    });
    stdcall(reg, K32, "SetCurrentDirectoryW", 1, |e| {
        let p = path_w(e, e.arg(0));
        set_current_directory(e, p)
    });
    stdcall(reg, K32, "GetFullPathNameA", 4, get_full_path_name_a);
    stdcall(reg, K32, "GetWindowsDirectoryA", 2, |e| {
        put_dir(e, "C:\\Windows")
    });
    stdcall(reg, K32, "GetSystemDirectoryA", 2, |e| {
        put_dir(e, "C:\\Windows\\system32")
    });
    stdcall(reg, K32, "GetTempPathA", 2, |e| {
        let (cap, buf) = (e.arg(0), e.arg(1));
        let p = format!(
            "C:\\Users\\{}\\AppData\\Local\\Temp\\",
            e.ext.get::<Win32>().cfg.user_name
        );
        match put_cstr(e.cpu, buf, cap, p.as_bytes()) {
            Some(n) => Outcome::Return(n),
            None => Outcome::Return(p.len() as u32 + 1),
        }
    });
    stdcall(
        reg,
        K32,
        "GetPrivateProfileStringA",
        6,
        get_private_profile_string_a,
    );
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
