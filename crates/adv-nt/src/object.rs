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
