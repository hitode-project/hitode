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
}

impl Attributes {
    pub fn read(env: &Env<'_>, address: u32) -> Result<Attributes, Error> {
        if address == 0 {
            return Err(status::INVALID_PARAMETER.into());
        }
        if read_u32(env, address)? != LENGTH {
            return Err(status::INVALID_PARAMETER.into());
        }
        if read_u32(env, address + SECURITY_DESCRIPTOR)? != 0 {
            return Err(Error::Unsupported(
                "object attributes with a security descriptor".into(),
            ));
        }
        let name = match read_u32(env, address + OBJECT_NAME)? {
            0 => None,
            name => Some(read_unicode_string(env, name)?),
        };
        Ok(Attributes {
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
        for value in [self.root, self.attributes, 0, 2 * name.len() as u32] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
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
        let attributes = Attributes::read(env, attributes)?;
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

/// NtCreateDirectoryObject(*handle, access, *attributes)
pub fn create_directory(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    let &[handle, access, attributes] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        let objattr = Attributes::read(env, attributes)?.wire();
        let mut request = crate::protocol::CreateDirectoryRequest {
            access,
            ..Default::default()
        };
        let (reply, _) = crate::server::request(env, &mut request, &[&objattr], &mut [])?;
        write_u32(env, handle, reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtCreateSymbolicLinkObject(*handle, access, *attributes, *target)
pub fn create_symbolic_link(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    let &[handle, access, attributes, target] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        if read_u32(env, target)? >> 16 == 0 {
            return Err(status::INVALID_PARAMETER.into());
        }
        let target: Vec<u8> = read_unicode_string(env, target)?
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        let objattr = Attributes::read(env, attributes)?.wire();
        let mut request = crate::protocol::CreateSymlinkRequest {
            access,
            ..Default::default()
        };
        let (reply, _) = crate::server::request(env, &mut request, &[&objattr, &target], &mut [])?;
        write_u32(env, handle, reply.handle)?;
        status::check(reply.header.error)
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
