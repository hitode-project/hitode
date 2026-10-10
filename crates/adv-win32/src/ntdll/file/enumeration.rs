//! Native directory objects keep one enumeration cursor across all handle aliases.
use super::*;
use adv_cpu::backend::GuestExt;
use adv_os::vfs::{DirEntry, wildcard};

pub(super) struct DirectoryObj {
    pub(super) path: String,
    pub(super) entries: Option<Vec<DirEntry>>,
    pub(super) next: usize,
    pub(super) access: u32,
    pub(super) share: u32,
    pub(super) backing: Option<adv_os::vfs::FileStream>,
}

const MAX_ROWS: usize = 32768;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

fn snapshot_bytes(entries: &[DirEntry]) -> usize {
    entries
        .iter()
        .map(|entry| std::mem::size_of::<DirEntry>() + entry.name.len())
        .sum()
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(query_ABI);
    registry.register(adv_abi::Api {
        name: "ZwQueryDirectoryFile",
        ..query_ABI
    });
}

fn expression(env: &Env<'_>, pointer: GuestPtr<()>) -> Result<String, i32> {
    if pointer.is_null() {
        return Ok("*".into());
    }
    check_range(env.cpu, pointer.addr(), 8, Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
    let sizes = env
        .cpu
        .read_u32(pointer.addr())
        .map_err(|_| ACCESS_VIOLATION)?;
    let length = sizes & 0xffff;
    let buffer = env
        .cpu
        .read_u32(pointer.addr() + 4)
        .map_err(|_| ACCESS_VIOLATION)?;
    if length & 1 != 0 || length > sizes >> 16 {
        return Err(INVALID_PARAMETER);
    }
    check_range(env.cpu, buffer, length, Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
    let mut bytes = vec![0; length as usize];
    env.cpu
        .read(buffer, &mut bytes)
        .map_err(|_| ACCESS_VIOLATION)?;
    let units: Vec<_> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let name = String::from_utf16(&units).map_err(|_| NAME_INVALID)?;
    if name.contains(['\0', '\\', '/']) {
        return Err(NAME_INVALID);
    }
    Ok(name)
}

fn time(value: Option<std::time::SystemTime>) -> u64 {
    value
        .and_then(|value| value.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |value| {
            116_444_736_000_000_000 + value.as_micros() as u64 * 10
        })
}

fn information(entry: &DirEntry, class: i32, index: usize) -> Vec<u8> {
    let name: Vec<_> = entry
        .name
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let offset = match class {
        1 => 64,
        2 => 68,
        3 => 94,
        12 => 12,
        _ => unreachable!(),
    };
    let mut data = vec![0; offset + name.len()];
    data[4..8].copy_from_slice(&(index as u32).to_le_bytes());
    if class == 12 {
        data[8..12].copy_from_slice(&(name.len() as u32).to_le_bytes());
    } else {
        for (offset, value) in [
            (8, entry.created),
            (16, entry.accessed),
            (24, entry.modified),
            (32, entry.modified),
        ] {
            data[offset..offset + 8].copy_from_slice(&time(value).to_le_bytes());
        }
        data[40..48].copy_from_slice(&entry.size.to_le_bytes());
        data[48..56].copy_from_slice(&entry.size.to_le_bytes());
        data[56..60].copy_from_slice(&entry.attributes.to_le_bytes());
        data[60..64].copy_from_slice(&(name.len() as u32).to_le_bytes());
    }
    data[offset..].copy_from_slice(&name);
    data
}

#[win32(ntdll, name = "NtQueryDirectoryFile", level = implement)]
pub(crate) fn query(
    env: &mut Env<'_>,
    handle: Handle,
    event: Handle,
    apc: GuestPtr<()>,
    context: GuestPtr<()>,
    iosb: GuestPtr<()>,
    output: GuestPtr<()>,
    length: u32,
    class: i32,
    single: u32,
    name: GuestPtr<()>,
    restart: u32,
) -> i32 {
    // ReturnSingleEntry and RestartScan are BOOLEANs: callers push a full
    // stack slot whose upper bytes are unspecified.
    let (single, restart) = (single as u8 != 0, restart as u8 != 0);
    if event.0 != 0 || !apc.is_null() || !context.is_null() {
        return unavailable_for(
            env,
            "ntdll.dll!NtQueryDirectoryFile",
            "asynchronous directory completion is unavailable",
        );
    }
    if !matches!(class, 1 | 2 | 3 | 12) {
        return unavailable_for(
            env,
            "ntdll.dll!NtQueryDirectoryFile",
            "directory information class is unavailable",
        );
    }
    if check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
        || check_range(env.cpu, output.addr(), length, Prot::WRITE).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let Some(directory) = env.sys.objects.host::<DirectoryObj>(handle.0) else {
        return if env.sys.objects.get(handle.0).is_some() {
            0xc000_0024u32 as i32
        } else {
            INVALID_HANDLE
        };
    };
    if directory.access & (GENERIC_READ | 1) == 0 {
        return ACCESS_DENIED;
    }
    let first = directory.entries.is_none();
    if first {
        let pattern = match expression(env, name) {
            Ok(pattern) => pattern,
            Err(status) => return status,
        };
        let path = directory.path.clone();
        let entries: Vec<_> = match env.ext.get::<Win32>().vfs.list(&path) {
            Some(entries) => entries
                .into_iter()
                .filter(|entry| wildcard(&pattern, &entry.name))
                .collect(),
            None => return ACCESS_DENIED,
        };
        let existing: usize = env
            .sys
            .objects
            .all_hosts::<DirectoryObj>()
            .filter_map(|(_, directory)| directory.entries.as_deref())
            .map(snapshot_bytes)
            .sum();
        if entries.len() > MAX_ROWS || existing + snapshot_bytes(&entries) > MAX_SNAPSHOT_BYTES {
            adv_abi::warn_unavailable(
                env,
                "ntdll.dll!NtQueryDirectoryFile",
                "directory snapshots are bounded to 32768 rows per cursor and 64MiB per process",
            );
            return INSUFFICIENT_RESOURCES;
        }
        let directory = env.sys.objects.host_mut::<DirectoryObj>(handle.0).unwrap();
        directory.entries = Some(entries);
    }
    let directory = env.sys.objects.host_mut::<DirectoryObj>(handle.0).unwrap();
    if restart {
        directory.next = 0;
    }
    let entries = directory.entries.as_ref().unwrap();
    if directory.next >= entries.len() {
        return completion(
            env,
            iosb,
            if first {
                0xc000_000fu32 as i32
            } else {
                0x8000_0006u32 as i32
            },
            0,
        );
    }
    let mut bytes = Vec::new();
    let mut next = directory.next;
    let mut previous = None;
    while let Some(entry) = entries.get(next) {
        let record = information(entry, class, next);
        let start = (bytes.len() + 7) & !7;
        if start + record.len() > length as usize {
            if bytes.is_empty() {
                // A first-call overflow returns the fixed structure and the
                // name prefix that fits, while keeping the cursor retryable.
                if first && length as usize >= record.len() - entry.name.encode_utf16().count() * 2
                {
                    let count = length as usize;
                    if env.cpu.write(output.addr(), &record[..count]).is_err() {
                        return ACCESS_VIOLATION;
                    }
                    return completion(env, iosb, 0x8000_0005u32 as i32, length);
                }
                return completion(env, iosb, if first { 0xc000_0004u32 as i32 } else { 0 }, 0);
            }
            break;
        }
        if let Some(previous) = previous {
            bytes[previous..previous + 4]
                .copy_from_slice(&((start - previous) as u32).to_le_bytes());
        }
        bytes.resize(start, 0);
        bytes.extend_from_slice(&record);
        previous = Some(start);
        next += 1;
        if single {
            break;
        }
    }
    if env.cpu.write(output.addr(), &bytes).is_err() {
        return ACCESS_VIOLATION;
    }
    env.sys
        .objects
        .host_mut::<DirectoryObj>(handle.0)
        .unwrap()
        .next = next;
    completion(env, iosb, 0, bytes.len() as u32)
}
