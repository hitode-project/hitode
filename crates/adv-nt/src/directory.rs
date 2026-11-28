//! Directory listings, after Wine's `NtQueryDirectoryFile`.
//!
//! The first query on a handle lists the directory: `.` and `..`, then
//! the entries that match the mask in name order. Each query goes on from
//! where the last stopped, until a restart. Entries include Wine's hashed
//! 8.3 aliases for names that need them. A mask without wildcards names
//! one entry, which the path index finds without listing the directory
//! again, as Wine finds it by name.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};
use adv_pe::{ImportName, Pe};
use iced_x86::{Code, Decoder, DecoderOptions};

use crate::file::basic_information;
use crate::object::{read_unicode_string, write_bytes, write_u32};
use crate::server;
use crate::status::{self, Error, Status};

const FILE_DIRECTORY_INFORMATION: u32 = 1;
const FILE_FULL_DIRECTORY_INFORMATION: u32 = 2;
const FILE_BOTH_DIRECTORY_INFORMATION: u32 = 3;
const FD_TYPE_DIR: i32 = 2;
const BUFFER_OVERFLOW: Status = 0x8000_0005;
const NO_MORE_FILES: Status = 0x8000_0006;
const FILE_ID_BOTH_DIRECTORY_INFORMATION: u32 = 37;
const FILE_ID_FULL_DIRECTORY_INFORMATION: u32 = 38;
const FILE_ID_EXTD_DIRECTORY_INFORMATION: u32 = 60;
const FILE_ID_EXTD_BOTH_DIRECTORY_INFORMATION: u32 = 63;

pub(crate) const FIND_QUERY: &str = "find_directory_query";

struct FindQuerySlot(u32);

/// Route Wine's Win32 file searches through the current guest import,
/// asking for the record Windows asks for. `FindFirstFileExW` and
/// `FindNextFileW` on Windows query `FileBothDirectoryInformation`; Wine
/// asks for `FileIdExtdBothDirectoryInformation`, which inline hooks written
/// against Windows answer with an empty success.
pub(crate) fn bind_find_query(env: &mut Env<'_>, base: u32, pe: &Pe) {
    let Some(table) = env
        .ext
        .get_or_default::<crate::identity::Images>()
        .0
        .get(&base)
        .filter(|module| module.name == "kernelbase.dll")
        .and_then(crate::identity::Module::table)
    else {
        return;
    };
    let (Ok(relocations), Ok(imports)) = (pe.relocations(), pe.imports()) else {
        return;
    };
    let Some(slot) = imports
        .into_iter()
        .filter(|module| module.dll.eq_ignore_ascii_case("ntdll.dll"))
        .flat_map(|module| module.imports)
        .find_map(|import| match import.name {
            ImportName::Name { name, .. } if name == "NtQueryDirectoryFile" => {
                Some(base + import.iat_rva)
            }
            _ => None,
        })
    else {
        return;
    };
    let mut calls = Vec::new();
    for name in ["FindFirstFileExW", "FindNextFileW"] {
        let Some(function) = table.named(name) else {
            return;
        };
        let Ok(code) = pe.slice(function.rva, function.size) else {
            return;
        };
        let entry = base + function.rva;
        if !crate::identity::intact(env.cpu, entry, function, |address| {
            relocations.contains(&address.wrapping_sub(base))
        }) {
            return;
        }
        let Some(call) = Decoder::with_ip(32, code, u64::from(entry), DecoderOptions::NONE)
            .into_iter()
            .find(|ins| {
                ins.code() == Code::Call_rm32
                    && ins.len() == 6
                    && ins.memory_displacement32() == slot
            })
        else {
            return;
        };
        calls.push(call.ip32());
    }
    let Ok(thunk) = env
        .sys
        .thunks
        .publish(env.cpu, crate::dispatch::MODULE, FIND_QUERY)
    else {
        return;
    };
    for at in calls {
        let mut bytes = [0xe8, 0, 0, 0, 0, 0x90];
        bytes[1..5].copy_from_slice(&thunk.wrapping_sub(at + 5).to_le_bytes());
        if env.cpu.write(at, &bytes).is_err() {
            return;
        }
    }
    env.ext.insert(FindQuerySlot(slot));
}

/// A `FileBothDirectoryInformation` record: the fixed part before the name.
const BOTH_FIXED: usize = 94;
/// Where `FileIdExtdBothDirectoryInformation` inserts its reparse tag and
/// 128-bit file id, which push the short and long names 20 bytes further.
const EXTD_INSERT: usize = 68;
const EXTD_GROWTH: usize = 20;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

pub(crate) fn find_query(env: &mut Env<'_>) -> Outcome {
    let slot = env.ext.get::<FindQuerySlot>().0;
    let Ok(target) = env.cpu.read_u32(slot) else {
        return Outcome::Return(status::ACCESS_VIOLATION);
    };
    let mut args: [u32; 11] = std::array::from_fn(|i| env.arg(i as u32));
    let (io, buffer, length) = (args[4], args[5], args[6]);
    // A record grows by at most 24 bytes once aligned, and the smallest
    // aligned record is 96 bytes: four fifths of the buffer always fit.
    args[6] = length / 5 * 4;
    args[7] = FILE_BOTH_DIRECTORY_INFORMATION;
    Outcome::call(target, args, move |env, result| {
        if result.value() != status::SUCCESS {
            return Outcome::Return(result.value());
        }
        status::finish(extend_records(env, io, buffer, length).map_err(Error::Status))
    })
}

/// Rewrites the `FileBothDirectoryInformation` records a query left in
/// `buffer` as the `FileIdExtdBothDirectoryInformation` records Wine reads.
/// The file id is not part of the Windows record and stays zero; the reparse
/// tag comes from `EaSize`, where the Windows record reports it.
fn extend_records(env: &mut Env<'_>, io: u32, buffer: u32, length: u32) -> Result<(), Status> {
    let size = env
        .cpu
        .read_u32(io + 4)
        .map_err(|_| status::ACCESS_VIOLATION)?;
    let mut old = vec![0; size as usize];
    env.cpu
        .read(buffer, &mut old)
        .map_err(|_| status::ACCESS_VIOLATION)?;
    let mut new = Vec::with_capacity(length as usize);
    let mut at = 0;
    while at < old.len() {
        let field = |offset: usize| {
            old.get(at + offset..at + offset + 4)
                .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()) as usize)
                .ok_or(status::INVALID_PARAMETER)
        };
        let (next, attributes, name, ea) = (field(0)?, field(56)?, field(60)?, field(64)?);
        let end = at + BOTH_FIXED + name;
        if end > old.len() || (next != 0 && at + next < end) {
            return Err(status::INVALID_PARAMETER);
        }
        let start = new.len();
        let tag = if attributes as u32 & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            ea
        } else {
            0
        };
        new.extend_from_slice(&old[at..at + EXTD_INSERT]);
        new.extend_from_slice(&(tag as u32).to_le_bytes());
        new.extend_from_slice(&[0; EXTD_GROWTH - 4]);
        new.extend_from_slice(&old[at + EXTD_INSERT..end]);
        if next == 0 {
            break;
        }
        new.resize(start + (new.len() - start).next_multiple_of(8), 0);
        let offset = (new.len() - start) as u32;
        new[start..start + 4].copy_from_slice(&offset.to_le_bytes());
        at += next;
    }
    if new.len() > length as usize {
        return Err(status::INVALID_PARAMETER);
    }
    env.cpu
        .write(buffer, &new)
        .map_err(|_| status::ACCESS_VIOLATION)?;
    write_u32(env, io + 4, new.len() as u32)
}

/// The listing of each directory handle that has been queried.
#[derive(Default)]
pub struct Listings(HashMap<u32, Listing>);

struct Listing {
    directory: PathBuf,
    names: Vec<OsString>,
    position: usize,
}

/// Forgets the listing of `handle`, which is closing.
pub fn forget(env: &mut Env<'_>, handle: u32) {
    if let Some(listings) = env.ext.try_get_mut::<Listings>() {
        listings.0.remove(&handle);
    }
}

/// NtQueryDirectoryFile(handle, event, apc, apc_context, *io, *buffer,
/// length, class, single_entry, *mask, restart_scan)
pub fn query(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[
        handle,
        event,
        apc,
        _,
        io,
        buffer,
        length,
        class,
        single,
        mask,
        restart,
    ] = args
    else {
        unreachable!()
    };
    // NT BOOLEAN arguments occupy a stack word, but only its low byte is defined.
    let single = single as u8 != 0;
    let restart = restart as u8 != 0;
    status::finish((|| {
        if event != 0 || apc != 0 {
            return Err(Error::Unsupported(
                "NtQueryDirectoryFile with an event or APC routine".into(),
            ));
        }
        let fixed = match fixed_record_size(class) {
            Some(fixed) => fixed,
            None if matches!(class, 10 | 12 | 50) => {
                return Err(Error::Unsupported(format!(
                    "directory information class {class}"
                )));
            }
            None => return Err(status::INVALID_INFO_CLASS.into()),
        };
        if length < align(fixed) {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        if buffer == 0 {
            return Err(status::ACCESS_VIOLATION.into());
        }
        let fd = server::handle_fd(env, handle)?;
        if fd.kind != FD_TYPE_DIR {
            return Err(status::INVALID_PARAMETER.into());
        }
        let directory = crate::section::file_path(fd.fd);
        drop(fd);
        let mounts = crate::file::volume::mounts(env);
        if let Some(directory) = &directory {
            mounts
                .prepare_directory(directory)
                .map_err(|_| status::OBJECT_PATH_NOT_FOUND)?;
        }
        let mask = match mask {
            0 => None,
            mask => Some(read_unicode_string(env, mask)?).filter(|mask| !mask.is_empty()),
        };

        let listings = &mut env.ext.get_or_default::<Listings>().0;
        let fresh = !listings.contains_key(&handle) || (restart && mask.is_some());
        if fresh {
            let directory = directory.ok_or(status::INVALID_PARAMETER)?;
            let names = list(&directory, mask.as_deref())?;
            if names.is_empty() {
                return Err(status::NO_SUCH_FILE.into());
            }
            let listing = Listing {
                directory,
                names,
                position: 0,
            };
            listings.insert(handle, listing);
        }
        let listing = listings.get_mut(&handle).expect("listed above");
        if restart {
            listing.position = 0;
        }

        let mut bytes = Vec::new();
        let mut last: Option<u32> = None;
        let mut result = status::SUCCESS;
        while listing.position < listing.names.len() {
            let name = &listing.names[listing.position];
            let path = listing.directory.join(name);
            let (Ok(link), Ok(metadata)) = (path.symlink_metadata(), path.metadata()) else {
                // Gone since the listing was made.
                listing.position += 1;
                continue;
            };
            let start = align(bytes.len() as u32);
            if start + fixed > length {
                break;
            }
            let name: Vec<u8> = name
                .to_string_lossy()
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            let room = (length - start - fixed) as usize;
            if last.is_some() && name.len() > room {
                break;
            }
            let shown = name.len().min(room);
            bytes.resize((start + fixed) as usize, 0);
            let entry = &mut bytes[start as usize..];
            let optical = mounts.metadata(&metadata);
            let basic = optical.as_ref().map_or_else(
                || basic_information(&path, &link, &metadata),
                |entry| entry.basic(),
            );
            entry[8..40].copy_from_slice(&basic[..32]);
            entry[40..48].copy_from_slice(&metadata.size().to_le_bytes());
            entry[48..56].copy_from_slice(
                &optical
                    .as_ref()
                    .map_or(metadata.blocks() * 512, |entry| entry.allocated)
                    .to_le_bytes(),
            );
            entry[56..60].copy_from_slice(&basic[32..36]);
            entry[60..64].copy_from_slice(&(name.len() as u32).to_le_bytes());
            write_record_ids_and_short_name(entry, class, metadata.ino(), &name);
            bytes.extend_from_slice(&name[..shown]);
            if let Some(previous) = last {
                bytes[previous as usize..previous as usize + 4]
                    .copy_from_slice(&(start - previous).to_le_bytes());
            }
            last = Some(start);
            listing.position += 1;
            if shown < name.len() {
                result = BUFFER_OVERFLOW;
                break;
            }
            if single {
                break;
            }
        }
        if last.is_none() {
            result = NO_MORE_FILES;
        }
        write_bytes(env, buffer, &bytes)?;
        write_u32(env, io, result)?;
        write_u32(env, io + 4, bytes.len() as u32)?;
        match result {
            status::SUCCESS => Ok(()),
            result => Err(result.into()),
        }
    })())
}

fn fixed_record_size(class: u32) -> Option<u32> {
    match class {
        FILE_DIRECTORY_INFORMATION => Some(64),
        FILE_FULL_DIRECTORY_INFORMATION => Some(68),
        FILE_BOTH_DIRECTORY_INFORMATION => Some(94),
        FILE_ID_BOTH_DIRECTORY_INFORMATION => Some(104),
        FILE_ID_FULL_DIRECTORY_INFORMATION => Some(80),
        FILE_ID_EXTD_DIRECTORY_INFORMATION => Some(88),
        FILE_ID_EXTD_BOTH_DIRECTORY_INFORMATION => Some(114),
        _ => None,
    }
}

fn write_record_ids_and_short_name(entry: &mut [u8], class: u32, ino: u64, name: &[u8]) {
    match class {
        FILE_ID_BOTH_DIRECTORY_INFORMATION => {
            entry[96..104].copy_from_slice(&ino.to_le_bytes());
        }
        FILE_ID_FULL_DIRECTORY_INFORMATION
        | FILE_ID_EXTD_DIRECTORY_INFORMATION
        | FILE_ID_EXTD_BOTH_DIRECTORY_INFORMATION => {
            entry[72..80].copy_from_slice(&ino.to_le_bytes());
        }
        _ => {}
    }
    if matches!(
        class,
        FILE_BOTH_DIRECTORY_INFORMATION
            | FILE_ID_BOTH_DIRECTORY_INFORMATION
            | FILE_ID_EXTD_BOTH_DIRECTORY_INFORMATION
    ) {
        let units: Vec<u16> = name
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let short = crate::path::short_name(&units);
        let offset = if class == FILE_ID_EXTD_BOTH_DIRECTORY_INFORMATION {
            88
        } else {
            68
        };
        entry[offset] = (short.len() * 2) as u8;
        for (index, unit) in short.iter().enumerate() {
            entry[offset + 2 + index * 2..offset + 4 + index * 2]
                .copy_from_slice(&unit.to_le_bytes());
        }
    }
}

fn align(size: u32) -> u32 {
    size.next_multiple_of(8)
}

/// `.`, `..` and the entries of `directory` that match `mask`, in order.
/// A mask without wildcards names one entry, found through the path index
/// as Wine finds it by name; other masks list the directory, checking only
/// matching names.
fn list(directory: &std::path::Path, mask: Option<&[u16]>) -> Result<Vec<OsString>, Status> {
    let listed = |path: &std::path::Path| {
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() || metadata.is_dir())
    };
    if let Some(mask) = mask
        && !mask.iter().any(|unit| WILDCARDS.contains(unit))
        && let Ok(name) = String::from_utf16(mask)
        && name != "."
        && name != ".."
    {
        let found = crate::path::entry(directory, &name);
        return Ok(found
            .into_iter()
            .filter(|name| listed(&directory.join(name)))
            .collect());
    }
    let wanted = |name: &OsString| {
        let name: Vec<u16> = name.to_string_lossy().encode_utf16().collect();
        mask.is_none_or(|mask| {
            matches(mask, &name) || matches(mask, &crate::path::short_name(&name))
        })
    };
    let entries = std::fs::read_dir(directory).map_err(|_| status::OBJECT_PATH_NOT_FOUND)?;
    let mut names: Vec<OsString> = entries
        .flatten()
        .filter(|entry| wanted(&entry.file_name()))
        .filter(|entry| match entry.file_type() {
            Ok(kind) if !kind.is_symlink() => kind.is_file() || kind.is_dir(),
            _ => listed(&entry.path()),
        })
        .map(|entry| entry.file_name())
        .collect();
    names.sort();
    let dots = [".", ".."].map(OsString::from);
    Ok(dots.into_iter().filter(wanted).chain(names).collect())
}

/// Match an NT search expression, including Wine's DOS wildcard tokens.
fn matches(mask: &[u16], name: &[u16]) -> bool {
    let upper = |c: u16| {
        char::from_u32(c.into())
            .map(|c| c.to_uppercase().next().unwrap_or(c) as u32)
            .unwrap_or(c.into())
    };
    let last_dot = name.iter().rposition(|&unit| unit == DOT);
    let mut previous = vec![false; name.len() + 1];
    let mut current = previous.clone();
    previous[0] = true;
    for &unit in mask {
        for position in 0..=name.len() {
            current[position] = match unit {
                STAR => previous[position] || position > 0 && current[position - 1],
                DOS_STAR => {
                    previous[position]
                        || position > 0 && Some(position - 1) != last_dot && current[position - 1]
                }
                DOS_QM => {
                    previous[position] && (position == name.len() || name[position] == DOT)
                        || position > 0 && name[position - 1] != DOT && previous[position - 1]
                }
                DOS_DOT => {
                    position == name.len() && previous[position]
                        || position > 0 && name[position - 1] == DOT && previous[position - 1]
                }
                _ => {
                    position > 0
                        && (unit == QUESTION || upper(unit) == upper(name[position - 1]))
                        && previous[position - 1]
                }
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[name.len()]
}

const STAR: u16 = b'*' as u16;
const QUESTION: u16 = b'?' as u16;
const DOT: u16 = b'.' as u16;
const DOS_STAR: u16 = b'<' as u16;
const DOS_QM: u16 = b'>' as u16;
const DOS_DOT: u16 = b'"' as u16;
const WILDCARDS: [u16; 5] = [STAR, QUESTION, DOS_STAR, DOS_QM, DOS_DOT];

#[cfg(test)]
mod tests {
    use super::{fixed_record_size, list, matches, write_record_ids_and_short_name};
    use std::ffi::OsString;

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn masks_match_without_regard_to_case() {
        assert!(matches(&utf16("*.TTF"), &utf16("tahoma.ttf")));
        assert!(matches(&utf16("t?homa.*"), &utf16("Tahoma.ttf")));
        assert!(matches(&utf16("*"), &utf16("")));
        assert!(!matches(&utf16("*.fon"), &utf16("tahoma.ttf")));
        assert!(!matches(&utf16("?"), &utf16("")));
    }

    // spec: WINE-SYS-5
    #[test]
    fn wine_dos_masks_enumerate_numbered_and_extensionless_files() {
        for (mask, name, expected) in [
            ("pack>>>>.arc", "pack0001.arc", true),
            ("patch>>.dat", "patch.dat", true),
            ("patch>>.dat", "patch12.dat", true),
            ("patch>>.dat", "patch123.dat", false),
            ("*\"*", "README", true),
            ("*\"*", "README.txt", true),
            ("<", "README", true),
            ("<", "README.txt", false),
            ("<.txt", "name.part.txt", true),
        ] {
            assert_eq!(
                matches(&utf16(mask), &utf16(name)),
                expected,
                "{mask}, {name}"
            );
        }
    }

    // spec: WINE-SYS-5
    #[test]
    fn directory_id_classes_match_windows_offsets() {
        assert_eq!(fixed_record_size(3), Some(94));
        assert_eq!(fixed_record_size(37), Some(104));
        assert_eq!(fixed_record_size(38), Some(80));
        assert_eq!(fixed_record_size(60), Some(88));
        assert_eq!(fixed_record_size(63), Some(114));

        let name: Vec<u8> = utf16("longfilename_test.txt")
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        let ino = 0x0018_0000_0005_b376u64;

        let mut e37 = vec![0u8; 104];
        write_record_ids_and_short_name(&mut e37, 37, ino, &name);
        assert_eq!(e37[68], 24);
        assert_eq!(&e37[96..104], &ino.to_le_bytes());

        let mut e38 = vec![0u8; 80];
        write_record_ids_and_short_name(&mut e38, 38, ino, &name);
        assert_eq!(&e38[72..80], &ino.to_le_bytes());

        let mut e60 = vec![0u8; 88];
        write_record_ids_and_short_name(&mut e60, 60, ino, &name);
        assert_eq!(&e60[72..80], &ino.to_le_bytes());
    }

    // spec: WINE-SYS-5
    #[test]
    fn exact_masks_name_one_entry_and_wildcards_list_files_and_directories() {
        let root = std::env::temp_dir().join(format!("adv-nt-directory-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Sub")).unwrap();
        std::fs::write(root.join("Present.G00"), b"").unwrap();
        std::fs::write(root.join("LongFileName.txt"), b"").unwrap();
        std::os::unix::fs::symlink("Present.G00", root.join("link.g00")).unwrap();
        std::os::unix::fs::symlink("gone", root.join("broken.g00")).unwrap();
        let fifo =
            std::ffi::CString::new(root.join("pipe.g00").into_os_string().into_encoded_bytes())
                .unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let names = |mask: &str| -> Vec<String> {
            list(&root, Some(&utf16(mask)))
                .unwrap()
                .into_iter()
                .map(|name: OsString| name.into_string().unwrap())
                .collect()
        };
        let alias =
            String::from_utf16(&crate::path::short_name(&utf16("LongFileName.txt"))).unwrap();
        assert_eq!(names("present.g00"), ["Present.G00"]);
        assert_eq!(names(&alias), ["LongFileName.txt"]);
        assert_eq!(names("SUB"), ["Sub"]);
        assert_eq!(names("link.g00"), ["link.g00"]);
        for absent in ["missing.g00", "broken.g00", "pipe.g00"] {
            assert!(names(absent).is_empty(), "{absent}");
        }
        assert_eq!(names("*.g00"), ["Present.G00", "link.g00"]);
        assert_eq!(names("."), ["."]);
        // A name the host adds later changes the directory and is found.
        std::fs::write(root.join("Later.g00"), b"").unwrap();
        assert_eq!(names("later.G00"), ["Later.g00"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
