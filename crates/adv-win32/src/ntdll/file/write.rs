//! Synchronous writes keep the common VFS owner, cursor and mapped-file coherence.
use super::*;
use std::io::{self, Write};

pub(super) fn register(registry: &mut Registry) {
    registry.register(write_ABI);
    registry.register(adv_abi::Api {
        name: "NtWriteFile",
        ..write_ABI
    });
}

struct CountedWriter<'a> {
    file: &'a mut adv_os::vfs::FileStream,
    written: u32,
}
impl Write for CountedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.file.write(bytes)?;
        self.written += count as u32;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[win32(ntdll, name = "ZwWriteFile", level = implement)]
fn write(
    env: &mut Env<'_>,
    handle: Handle,
    event: Handle,
    apc: GuestPtr<()>,
    context: GuestPtr<()>,
    iosb: GuestPtr<()>,
    buffer: GuestPtr<()>,
    length: u32,
    offset: GuestPtr<()>,
    key: GuestPtr<u32>,
) -> i32 {
    if event.0 != 0 || !apc.is_null() || !context.is_null() || !key.is_null() {
        return unavailable_for(
            env,
            "ntdll.dll!ZwWriteFile",
            "asynchronous/event/APC completion and range-lock keys are unavailable",
        );
    }
    if check_range(env.cpu, iosb.addr(), 8, Prot::WRITE).is_err()
        || check_range(env.cpu, buffer.addr(), length, Prot::READ).is_err()
    {
        return ACCESS_VIOLATION;
    }
    let requested = if offset.is_null() {
        None
    } else {
        if check_range(env.cpu, offset.addr(), 8, Prot::READ).is_err() {
            return ACCESS_VIOLATION;
        }
        let mut bytes = [0; 8];
        if env.cpu.read(offset.addr(), &mut bytes).is_err() {
            return ACCESS_VIOLATION;
        }
        match i64::from_le_bytes(bytes) {
            -2 => None,
            -1 => {
                return unavailable_for(
                    env,
                    "ntdll.dll!ZwWriteFile",
                    "write-to-end and append-only semantics are unavailable",
                );
            }
            value if value >= 0 => Some(value as u64),
            _ => return INVALID_PARAMETER,
        }
    };
    let Some(file) = env.sys.objects.host_mut::<FileObj>(handle.0) else {
        return if env.sys.objects.get(handle.0).is_some()
            || matches!(handle.0, 0xffff_ffff | 0xffff_fffe)
        {
            0xc000_0024u32 as i32
        } else {
            INVALID_HANDLE
        };
    };
    if file.access & (GENERIC_WRITE | 0x1000_0000 | 2) == 0 {
        return if file.access & 4 != 0 {
            unavailable_for(
                env,
                "ntdll.dll!ZwWriteFile",
                "append-only file handles require append write support",
            )
        } else {
            ACCESS_DENIED
        };
    }
    if length == 0 {
        return completion(env, iosb, 0, 0);
    }
    let mut data = Vec::new();
    if data.try_reserve_exact(length as usize).is_err() {
        return INSUFFICIENT_RESOURCES;
    }
    data.resize(length as usize, 0);
    if env.cpu.read(buffer.addr(), &mut data).is_err() {
        return ACCESS_VIOLATION;
    }
    let start = match requested {
        Some(position) => position,
        None => match file.file.stream_position() {
            Ok(position) => position,
            Err(_) => return completion(env, iosb, 0xc000_0001u32 as i32, 0),
        },
    };
    if start
        .checked_add(u64::from(length))
        .is_none_or(|end| end > i64::MAX as u64)
    {
        return INVALID_PARAMETER;
    }
    if requested.is_some() && file.file.seek(SeekFrom::Start(start)).is_err() {
        return completion(env, iosb, INVALID_PARAMETER, 0);
    }
    let sections = &env.ext.get::<Win32>().vfs.sections;
    let content = if sections.has_entries() {
        file.file
            .cached_identity()
            .and_then(|identity| sections.lookup(identity))
    } else {
        None
    };
    let mut writer = CountedWriter {
        file: &mut file.file,
        written: 0,
    };
    let result = match content {
        Some(content) => content.write_to(&mut writer, start, &data),
        None => writer.write_all(&data),
    };
    let count = writer.written;
    let status = match result {
        Ok(()) => 0,
        Err(error) => match error.kind() {
            io::ErrorKind::PermissionDenied => ACCESS_DENIED,
            io::ErrorKind::InvalidInput => INVALID_PARAMETER,
            io::ErrorKind::OutOfMemory => INSUFFICIENT_RESOURCES,
            io::ErrorKind::StorageFull => 0xc000_007fu32 as i32,
            _ => 0xc000_0001u32 as i32,
        },
    };
    completion(env, iosb, status, count)
}
