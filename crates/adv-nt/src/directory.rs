//! Directory listings, after Wine's `NtQueryDirectoryFile`.
//!
//! The first query on a handle lists the directory: `.` and `..`, then
//! the entries that match the mask in name order. Each query goes on from
//! where the last stopped, until a restart. Entries include Wine's hashed
//! 8.3 aliases for names that need them.

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
const FILE_ID_EXTD_BOTH_DIRECTORY_INFORMATION: u32 = 63;

pub(crate) const FIND_QUERY: &str = "find_first_directory_query";

struct FindQuerySlot(u32);

/// Route Wine's initial Win32 file search through the current guest import.
/// Some inline hooks handle the older directory record but return empty
/// success for the extended record Wine requests.
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
    let Some(function) = table.named("FindFirstFileExW") else {
        return;
    };
    let (Ok(relocations), Ok(code), Ok(imports)) = (
        pe.relocations(),
        pe.slice(function.rva, function.size),
        pe.imports(),
    ) else {
        return;
    };
    let entry = base + function.rva;
    if !crate::identity::intact(env.cpu, entry, function, |address| {
        relocations.contains(&address.wrapping_sub(base))
    }) {
        return;
    }
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
    let Some(call) = Decoder::with_ip(32, code, u64::from(entry), DecoderOptions::NONE)
        .into_iter()
        .find(|ins| {
            ins.code() == Code::Call_rm32 && ins.len() == 6 && ins.memory_displacement32() == slot
        })
    else {
        return;
    };
    let Ok(thunk) = env
        .sys
        .thunks
        .publish(env.cpu, crate::dispatch::MODULE, FIND_QUERY)
    else {
        return;
    };
    let at = call.ip32();
    let mut bytes = [0xe8, 0, 0, 0, 0, 0x90];
    bytes[1..5].copy_from_slice(&thunk.wrapping_sub(at + 5).to_le_bytes());
    if env.cpu.write(at, &bytes).is_ok() {
        env.ext.insert(FindQuerySlot(slot));
    }
}

pub(crate) fn find_query(env: &mut Env<'_>) -> Outcome {
    let slot = env.ext.get::<FindQuerySlot>().0;
    let Ok(target) = env.cpu.read_u32(slot) else {
        return Outcome::Return(status::ACCESS_VIOLATION);
    };
    let mut args: [u32; 11] = std::array::from_fn(|i| env.arg(i as u32));
    let (io, buffer, length, class, mask, restart) =
        (args[4], args[5], args[6], args[7], args[9], args[10]);
    Outcome::call(target, args, move |env, result| {
        if result.value() != status::SUCCESS
            || class != FILE_ID_EXTD_BOTH_DIRECTORY_INFORMATION
            || restart as u8 == 0
            || env.cpu.read_u32(io + 4).ok() != Some(0)
            || !read_unicode_string(env, mask).is_ok_and(|name| {
                !name.is_empty()
                    && !name
                        .iter()
                        .any(|&unit| [42, 63, 60, 62, 34].contains(&unit))
            })
        {
            return Outcome::Return(result.value());
        }
        args[7] = FILE_BOTH_DIRECTORY_INFORMATION;
        Outcome::call(target, args, move |env, legacy| {
            if legacy.value() == status::SUCCESS
                && let Some(bytes) = legacy_find_record(env, io, buffer, length)
                && env.cpu.write(buffer, &bytes).is_ok()
            {
                return status::finish(
                    write_u32(env, io + 4, bytes.len() as u32).map_err(Error::Status),
                );
            }
            // Leave the result of the original empty query intact.
            status::finish(
                write_u32(env, io, status::SUCCESS)
                    .and_then(|()| write_u32(env, io + 4, 0))
                    .map_err(Error::Status),
            )
        })
    })
}

fn legacy_find_record(env: &Env<'_>, io: u32, buffer: u32, length: u32) -> Option<Vec<u8>> {
    let size = env.cpu.read_u32(io + 4).ok()?;
    if size < 94 || size > length.checked_sub(20)? {
        return None;
    }
    let mut entry = vec![0; size as usize];
    env.cpu.read(buffer, &mut entry).ok()?;
    let name_len = u32::from_le_bytes(entry[60..64].try_into().unwrap());
    if entry[..4] != [0; 4] || name_len > size - 94 || name_len > 518 || entry[68] > 24 {
        return None;
    }
    entry.splice(68..68, [0; 20]);
    Some(entry)
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
        let fixed = match class {
            FILE_DIRECTORY_INFORMATION => 64,
            FILE_FULL_DIRECTORY_INFORMATION => 68,
            FILE_BOTH_DIRECTORY_INFORMATION => 94,
            FILE_ID_EXTD_BOTH_DIRECTORY_INFORMATION => 114,
            10 | 12 | 37 | 38 | 50 | 60 => {
                return Err(Error::Unsupported(format!(
                    "directory information class {class}"
                )));
            }
            _ => return Err(status::INVALID_INFO_CLASS.into()),
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
            if class == 63 {
                entry[72..80].copy_from_slice(&metadata.ino().to_le_bytes());
            }
            if class == 3 || class == 63 {
                let units: Vec<u16> = name
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes(*pair))
                    .collect();
                let short = crate::path::short_name(&units);
                let offset = if class == 3 { 68 } else { 88 };
                entry[offset] = (short.len() * 2) as u8;
                for (index, unit) in short.iter().enumerate() {
                    entry[offset + 2 + index * 2..offset + 4 + index * 2]
                        .copy_from_slice(&unit.to_le_bytes());
                }
            }
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

fn align(size: u32) -> u32 {
    size.next_multiple_of(8)
}

/// `.`, `..` and the entries of `directory` that match `mask`, in order.
fn list(directory: &std::path::Path, mask: Option<&[u16]>) -> Result<Vec<OsString>, Status> {
    let wanted = |name: &OsString| {
        let name: Vec<u16> = name.to_string_lossy().encode_utf16().collect();
        mask.is_none_or(|mask| {
            matches(mask, &name) || matches(mask, &crate::path::short_name(&name))
        })
    };
    let entries = std::fs::read_dir(directory).map_err(|_| status::OBJECT_PATH_NOT_FOUND)?;
    let mut names: Vec<OsString> = entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .metadata()
                .is_ok_and(|metadata| metadata.is_file() || metadata.is_dir())
        })
        .map(|entry| entry.file_name())
        .filter(&wanted)
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

#[cfg(test)]
mod tests {
    use super::matches;

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
}
