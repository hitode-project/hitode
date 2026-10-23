//! Guest arguments shared by object system calls: OBJECT_ATTRIBUTES,
//! UNICODE_STRING, and the `object_attributes` form the server takes.

use adv_cpu::backend::GuestExt;
use adv_machine::Env;

use crate::protocol::OpenDirectoryRequest;
use crate::status::{self, Error, Status};

/// OBJECT_ATTRIBUTES (i386).
const LENGTH: u32 = 24;
const ROOT_DIRECTORY: u32 = 4;
const OBJECT_NAME: u32 = 8;
const ATTRIBUTES: u32 = 12;
const SECURITY_DESCRIPTOR: u32 = 16;

pub fn read_u32(env: &Env<'_>, address: u32) -> Result<u32, Status> {
    env.cpu
        .read_u32(address)
        .map_err(|_| status::ACCESS_VIOLATION)
}

pub fn write_u32(env: &mut Env<'_>, address: u32, value: u32) -> Result<(), Status> {
    env.cpu
        .write_u32(address, value)
        .map_err(|_| status::ACCESS_VIOLATION)
}

pub fn read_bytes(env: &Env<'_>, address: u32, length: u32) -> Result<Vec<u8>, Status> {
    let mut bytes = vec![0; length as usize];
    env.cpu
        .read(address, &mut bytes)
        .map_err(|_| status::ACCESS_VIOLATION)?;
    Ok(bytes)
}

pub fn write_bytes(env: &mut Env<'_>, address: u32, bytes: &[u8]) -> Result<(), Status> {
    env.cpu
        .write(address, bytes)
        .map_err(|_| status::ACCESS_VIOLATION)
}

/// The UTF-16 text of the UNICODE_STRING at `address`.
pub fn read_unicode_string(env: &Env<'_>, address: u32) -> Result<Vec<u16>, Status> {
    let length = read_u32(env, address)? & 0xffff;
    let buffer = read_u32(env, address + 4)?;
    if length % 2 != 0 {
        return Err(status::OBJECT_NAME_INVALID);
    }
    let mut bytes = vec![0; length as usize];
    env.cpu
        .read(buffer, &mut bytes)
        .map_err(|_| status::ACCESS_VIOLATION)?;
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect())
}

/// What an OBJECT_ATTRIBUTES names.
pub struct Attributes {
    pub root: u32,
    pub attributes: u32,
    pub name: Option<Vec<u16>>,
    pub security: Vec<u8>,
}

impl Attributes {
    pub fn read(env: &Env<'_>, address: u32) -> Result<Attributes, Error> {
        if address == 0 {
            return Err(status::INVALID_PARAMETER.into());
        }
        if read_u32(env, address)? != LENGTH {
            return Err(status::INVALID_PARAMETER.into());
        }
        let security = security_descriptor(env, read_u32(env, address + SECURITY_DESCRIPTOR)?)?;
        let name = match read_u32(env, address + OBJECT_NAME)? {
            0 => None,
            name => {
                if read_u32(env, name + 4)? & 1 != 0 {
                    return Err(0x8000_0002.into());
                }
                Some(read_unicode_string(env, name)?)
            }
        };
        if name.is_none() && read_u32(env, address + ROOT_DIRECTORY)? != 0 {
            return Err(status::OBJECT_NAME_INVALID.into());
        }
        Ok(Attributes {
            security,
            root: read_u32(env, address + ROOT_DIRECTORY)?,
            attributes: read_u32(env, address + ATTRIBUTES)?,
            name,
        })
    }

    /// The name as the server takes it.
    pub fn name_bytes(&self) -> Vec<u8> {
        let name = self.name.as_deref().unwrap_or_default();
        name.iter().flat_map(|unit| unit.to_le_bytes()).collect()
    }

    /// `struct object_attributes` followed by the name, DWORD-aligned.
    pub fn wire(&self) -> Vec<u8> {
        let name = self.name.as_deref().unwrap_or_default();
        let mut bytes = Vec::with_capacity(16 + 2 * name.len());
        for value in [
            self.root,
            self.attributes,
            self.security.len() as u32,
            2 * name.len() as u32,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&self.security);
        for unit in name {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.resize(bytes.len().next_multiple_of(4), 0);
        bytes
    }
}

/// NtClose(handle)
pub fn close(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    crate::directory::forget(env, args[0]);
    status::finish(crate::server::close(env, args[0]))
}

/// NtOpenDirectoryObject(*handle, access, *attributes)
pub fn open_directory(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    let &[handle, access, attributes] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        let attributes = crate::sync::open_attributes(env, attributes)?;
        let mut request = OpenDirectoryRequest {
            access,
            attributes: attributes.attributes,
            rootdir: attributes.root,
            ..Default::default()
        };
        let (reply, _) =
            crate::server::request(env, &mut request, &[&attributes.name_bytes()], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, handle, reply.handle)?;
        Ok(())
    })())
}

/// NtQueryObject(handle, class, *info, length, *used_length)
pub fn query(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    use crate::protocol::{GetObjectInfoRequest, GetObjectNameRequest, GetObjectTypeRequest};
    let &[handle, class, info, length, used_length] = args else {
        unreachable!()
    };
    status::finish((|| {
        if used_length != 0 {
            write_u32(env, used_length, 0)?;
        }
        let used = match class {
            4 => {
                if length < 2 {
                    return Err(0xc000_0206.into());
                }
                let mut request = crate::protocol::SetHandleInfoRequest {
                    handle,
                    ..Default::default()
                };
                let (reply, _) = crate::server::request(env, &mut request, &[], &mut [])?;
                status::check(reply.header.error)?;
                write_bytes(
                    env,
                    info,
                    &[
                        u8::from(reply.old_flags & 1 != 0),
                        u8::from(reply.old_flags & 2 != 0),
                    ],
                )?;
                2
            }
            0 => {
                if length < 56 {
                    return Err(status::INFO_LENGTH_MISMATCH.into());
                }
                let mut request = GetObjectInfoRequest {
                    handle,
                    ..Default::default()
                };
                let (reply, _) = crate::server::request(env, &mut request, &[], &mut [])?;
                status::check(reply.header.error)?;
                let mut bytes = [0; 56];
                for (offset, value) in [
                    (4, reply.access),
                    (8, reply.handle_count),
                    (12, reply.ref_count),
                ] {
                    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                write_bytes(env, info, &bytes)?;
                56
            }
            1 => {
                let mut name = vec![0; length.saturating_sub(10) as usize];
                let mut request = GetObjectNameRequest {
                    handle,
                    ..Default::default()
                };
                let (reply, count) = crate::server::request(env, &mut request, &[], &mut name)?;
                let mut result = reply.header.error;
                let used = if result == status::SUCCESS {
                    if reply.total == 0 {
                        if length < 8 {
                            result = status::INFO_LENGTH_MISMATCH;
                        } else {
                            write_bytes(env, info, &[0; 8])?;
                        }
                        8
                    } else {
                        write_u32(env, info, (count as u32) | (((count + 2) as u32) << 16))?;
                        write_u32(env, info + 4, info + 8)?;
                        write_bytes(env, info + 8, &name[..count])?;
                        write_bytes(env, info + 8 + count as u32, &[0; 2])?;
                        10 + count as u32
                    }
                } else if result == status::INFO_LENGTH_MISMATCH || result == 0x8000_0005 {
                    if length < 8 {
                        result = status::INFO_LENGTH_MISMATCH;
                    }
                    10 + reply.total
                } else {
                    return Err(result.into());
                };
                if used_length != 0 {
                    write_u32(env, used_length, used)?;
                }
                status::check(result)?;
                return Ok(());
            }
            2 => {
                // object_type_info followed by at most 32 WCHARs, as Wine requests it.
                let mut buffer = [0; 44 + 64];
                let mut request = GetObjectTypeRequest {
                    handle,
                    ..Default::default()
                };
                let (reply, _) = crate::server::request(env, &mut request, &[], &mut buffer)?;
                status::check(reply.header.error)?;
                let field =
                    |offset| u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap());
                let name_length = field(0);
                let used = 96 + name_length + 2;
                if used_length != 0 {
                    write_u32(env, used_length, used)?;
                }
                if length < used {
                    return Err(status::INFO_LENGTH_MISMATCH.into());
                }
                let mut bytes = vec![0; used as usize];
                for (offset, value) in [
                    (0, name_length | ((name_length + 2) << 16)),
                    (4, info + 96),
                    (8, field(8)),
                    (12, field(12)),
                    (32, field(16)),
                    (36, field(20)),
                    (60, field(28)),
                    (64, field(32)),
                    (68, field(36)),
                    (72, field(40)),
                    (76, field(24)),
                ] {
                    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                bytes[82] = (field(4) + 2) as u8;
                bytes[96..96 + name_length as usize]
                    .copy_from_slice(&buffer[44..44 + name_length as usize]);
                write_bytes(env, info, &bytes)?;
                used
            }
            class => {
                return Err(Error::Unsupported(format!(
                    "object information class {class}"
                )));
            }
        };
        if used_length != 0 {
            write_u32(env, used_length, used)?;
        }
        Ok(())
    })())
}

/// NtDuplicateObject(source_process, source, destination_process, *destination,
/// access, attributes, options)
pub fn duplicate(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    let &[
        src_process,
        src_handle,
        dst_process,
        destination,
        access,
        attributes,
        options,
    ] = args
    else {
        unreachable!()
    };
    status::finish((|| {
        if destination != 0 {
            write_u32(env, destination, 0)?;
        }
        let mut request = crate::protocol::DupHandleRequest {
            src_process,
            src_handle,
            dst_process,
            access,
            attributes,
            options,
            ..Default::default()
        };
        let (reply, _) = crate::server::request(env, &mut request, &[], &mut [])?;
        if destination != 0 {
            write_u32(env, destination, reply.handle)?;
        }
        status::check(reply.header.error)
    })())
}

/// NtQueryDirectoryObject(handle, *buffer, size, single, restart, *context, *used)
pub fn query_directory(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    let &[handle, buffer, size, single, restart, context, used] = args else {
        unreachable!()
    };
    status::finish((|| {
        let index = if restart as u8 != 0 {
            0
        } else {
            read_u32(env, context)?
        };
        let mut entries = vec![0; size as usize];
        let mut request = crate::protocol::GetDirectoryEntriesRequest {
            handle,
            index,
            max_count: if single as u8 != 0 { 1 } else { u32::MAX },
            ..Default::default()
        };
        let (reply, _) = crate::server::request(env, &mut request, &[], &mut entries)?;
        let mut result = reply.header.error;
        if result != 0 && result != 0x105 {
            return Err(result.into());
        }
        let word = |at: usize| u32::from_le_bytes(entries[at..at + 4].try_into().unwrap()) as usize;
        let mut count = 0;
        let mut needed = 16;
        let mut pos = 0;
        for _ in 0..reply.count {
            let name = word(pos);
            let kind = word(pos + 4);
            let entry_size = 16 + name + kind + 4;
            if needed + entry_size > size as usize {
                result = 0x105;
                break;
            }
            count += 1;
            needed += entry_size;
            pos += 8 + (name + kind).next_multiple_of(4);
        }
        let mut head = 16 * (count + 1);
        let mut bytes = vec![0; needed];
        pos = 0;
        for i in 0..count {
            let name = word(pos);
            let kind = word(pos + 4);
            let mut source = pos + 8;
            for (offset, length) in [(i * 16, name), (i * 16 + 8, kind)] {
                bytes[offset..offset + 4].copy_from_slice(
                    &((length as u32) | (((length + 2) as u32) << 16)).to_le_bytes(),
                );
                bytes[offset + 4..offset + 8]
                    .copy_from_slice(&(buffer + head as u32).to_le_bytes());
                bytes[head..head + length].copy_from_slice(&entries[source..source + length]);
                head += length + 2;
                source += length;
            }
            pos += 8 + (name + kind).next_multiple_of(4);
        }
        if size >= 16 {
            write_bytes(env, buffer, &bytes)?;
        }
        if reply.count == 0 && result == 0 {
            if used != 0 {
                write_u32(env, used, 16)?;
            }
            return Err(0x8000_001a.into());
        }
        if single as u8 != 0 && count == 0 {
            if used != 0 {
                write_u32(env, used, 36 + reply.total_len)?;
            }
            return Err(0xc000_0023.into());
        }
        write_u32(env, context, index + count as u32)?;
        if used != 0 {
            write_u32(env, used, head as u32)?;
        }
        status::check(result)
    })())
}

/// Serializes the descriptor's SID and ACL components as Wine's alloc_object_attributes.
fn security_descriptor(env: &Env<'_>, address: u32) -> Result<Vec<u8>, Error> {
    if address == 0 {
        return Ok(Vec::new());
    }
    let header = read_u32(env, address)?;
    if header & 0xff != 1 {
        return Err(0xc000_0058.into());
    }
    let control = header >> 16;
    let mut bytes = vec![0; 20];
    bytes[..4].copy_from_slice(&(control & !0x8000).to_le_bytes());
    for (index, offset) in [4, 8, 12, 16].into_iter().enumerate() {
        let pointer = read_u32(env, address + offset)?;
        if pointer == 0 || index == 2 && control & 0x10 == 0 || index == 3 && control & 4 == 0 {
            continue;
        }
        let pointer = if control & 0x8000 != 0 {
            address + pointer
        } else {
            pointer
        };
        let value = read_u32(env, pointer)?;
        let length = if index < 2 {
            8 + ((value >> 8) & 0xff) * 4
        } else {
            value >> 16
        };
        bytes[4 + index * 4..8 + index * 4].copy_from_slice(&length.to_le_bytes());
        let start = bytes.len();
        bytes.resize(start + length as usize, 0);
        env.cpu
            .read(pointer, &mut bytes[start..])
            .map_err(|_| status::ACCESS_VIOLATION)?;
    }
    bytes.resize(bytes.len().next_multiple_of(2), 0);
    Ok(bytes)
}

/// NtSetInformationObject(handle, class, info, length)
pub fn set(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    status::finish((|| {
        if args[1] != 4 {
            return Err(status::NOT_IMPLEMENTED.into());
        }
        if args[3] < 2 {
            return Err(0xc000_0206.into());
        }
        let mut bytes = [0; 2];
        env.cpu
            .read(args[2], &mut bytes)
            .map_err(|_| status::ACCESS_VIOLATION)?;
        let mut request = crate::protocol::SetHandleInfoRequest {
            handle: args[0],
            flags: i32::from(bytes[0] != 0) | (i32::from(bytes[1] != 0) << 1),
            mask: 3,
            ..Default::default()
        };
        let (reply, _) = crate::server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)
    })())
}

/// NtCreateDirectoryObject(*handle, access, *attributes)
pub fn create_directory(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attr = if args[2] == 0 {
            Vec::new()
        } else {
            Attributes::read(env, args[2])?.wire()
        };
        let mut request = crate::protocol::CreateDirectoryRequest {
            access: args[1],
            ..Default::default()
        };
        let (reply, _) = crate::server::request(env, &mut request, &[&attr], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtCreateSymbolicLinkObject(*handle, access, *attributes, *target)
pub fn create_symlink(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        if read_u32(env, args[3])? >> 16 == 0 {
            return Err(status::INVALID_PARAMETER.into());
        }
        if read_u32(env, args[3] + 4)? == 0 {
            return Err(status::ACCESS_VIOLATION.into());
        }
        let attr = if args[2] == 0 {
            Vec::new()
        } else {
            Attributes::read(env, args[2])?.wire()
        };
        let target: Vec<u8> = read_unicode_string(env, args[3])?
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        let mut request = crate::protocol::CreateSymlinkRequest {
            access: args[1],
            ..Default::default()
        };
        let (reply, _) = crate::server::request(env, &mut request, &[&attr, &target], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtOpenSymbolicLinkObject(*handle, access, *attributes)
pub fn open_symlink(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attr = crate::sync::open_attributes(env, args[2])?;
        let mut request = crate::protocol::OpenSymlinkRequest {
            access: args[1],
            attributes: attr.attributes,
            rootdir: attr.root,
            ..Default::default()
        };
        let (reply, _) = crate::server::request(env, &mut request, &[&attr.name_bytes()], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtQuerySymbolicLinkObject(handle, *target, *length)
pub fn query_symlink(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    status::finish((|| {
        if args[1] == 0 {
            return Err(status::ACCESS_VIOLATION.into());
        }
        let maximum = read_u32(env, args[1])? >> 16;
        let mut bytes = vec![0; maximum.saturating_sub(2) as usize];
        let mut request = crate::protocol::QuerySymlinkRequest {
            handle: args[0],
            ..Default::default()
        };
        let (reply, size) = crate::server::request(env, &mut request, &[], &mut bytes)?;
        if args[2] != 0 && matches!(reply.header.error, 0 | 0xc000_0023) {
            write_u32(env, args[2], reply.total + 2)?;
        }
        status::check(reply.header.error)?;
        let output = read_u32(env, args[1] + 4)?;
        write_bytes(env, output, &bytes[..size])?;
        write_bytes(env, output + size as u32, &[0; 2])?;
        write_bytes(env, args[1], &(size as u16).to_le_bytes())?;
        Ok(())
    })())
}
