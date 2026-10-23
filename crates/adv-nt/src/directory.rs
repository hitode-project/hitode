//! Directory listings, after Wine's `NtQueryDirectoryFile`.
//!
//! The first query on a handle lists the directory: `.` and `..`, then
//! the entries that match the mask in name order. Each query goes on from
//! where the last stopped, until a restart. Entries carry no 8.3 names, as
//! on a volume that does not make them.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use adv_machine::{Env, Outcome};

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
            63 => 114,
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
        let mask = match mask {
            0 => None,
            mask => Some(read_unicode_string(env, mask)?).filter(|mask| !mask.is_empty()),
        };

        let listings = &mut env.ext.get_or_default::<Listings>().0;
        let fresh = !listings.contains_key(&handle) || (restart != 0 && mask.is_some());
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
        if restart != 0 {
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
            let basic = basic_information(&path, &link, &metadata);
            entry[8..40].copy_from_slice(&basic[..32]);
            entry[40..48].copy_from_slice(&metadata.size().to_le_bytes());
            entry[48..56].copy_from_slice(&(metadata.blocks() * 512).to_le_bytes());
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
            if single != 0 {
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

/// Whether `name` matches `mask`, where `*` stands for any run of
/// characters and `?` for one, without regard to case.
fn matches(mask: &[u16], name: &[u16]) -> bool {
    let upper = |c: u16| {
        char::from_u32(c.into())
            .map(|c| c.to_uppercase().next().unwrap_or(c) as u32)
            .unwrap_or(c.into())
    };
    match mask.split_first() {
        None => name.is_empty(),
        Some((&STAR, rest)) => (0..=name.len()).any(|skip| matches(rest, &name[skip..])),
        Some((&c, rest)) => name.split_first().is_some_and(|(&n, name)| {
            (c == QUESTION || upper(c) == upper(n)) && matches(rest, name)
        }),
    }
}

const STAR: u16 = b'*' as u16;
const QUESTION: u16 = b'?' as u16;

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
}
