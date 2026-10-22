//! Registry system calls, after Wine's `unix/registry.c`: the server holds
//! the registry; these calls pass the request on and lay out the reply.

use adv_machine::{Env, Outcome};

use crate::object::{Attributes, read_unicode_string, write_bytes, write_u32};
use crate::protocol::{CreateKeyRequest, GetKeyValueRequest, OpenKeyRequest, SetKeyValueRequest};
use crate::server;
use crate::status::{self, Error, Status};

const OBJ_OPENIF: u32 = 0x80;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;
const OBJECT_NAME_EXISTS: Status = 0x4000_0000;
const OBJECT_PATH_SYNTAX_BAD: Status = 0xc000_003b;
const BUFFER_OVERFLOW: Status = 0x8000_0005;
const BUFFER_TOO_SMALL: Status = 0xc000_0023;
const REG_CREATED_NEW_KEY: u32 = 1;
const REG_OPENED_EXISTING_KEY: u32 = 2;
/// The longest value name, in bytes.
const MAX_VALUE_LENGTH: usize = 16383 * 2;

const KEY_VALUE_BASIC: u32 = 0;
const KEY_VALUE_FULL: u32 = 1;
const KEY_VALUE_PARTIAL: u32 = 2;
const KEY_VALUE_PARTIAL_ALIGN64: u32 = 3;

/// NtOpenKey(*key, access, *attributes)
pub fn open(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish(open_key(env, args[0], args[1], args[2]))
}

/// NtOpenKeyEx(*key, access, *attributes, options)
pub fn open_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish(open_key(env, args[0], args[1], args[2]))
}

fn open_key(env: &mut Env<'_>, key: u32, access: u32, attributes: u32) -> Result<(), Error> {
    write_u32(env, key, 0)?;
    let attributes = Attributes::read(env, attributes)?;
    let name = attributes.name.unwrap_or_default();
    let mut request = OpenKeyRequest {
        parent: attributes.root,
        access,
        attributes: attributes.attributes | OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let (reply, _) = server::request(env, &mut request, &[&bytes(&name)], &mut [])?;
    status::check(reply.header.error)?;
    write_u32(env, key, reply.hkey)?;
    Ok(())
}

/// NtCreateKey(*key, access, *attributes, index, *class, options, *disposition)
pub fn create(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[key, access, attributes, _, class, options, disposition] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, key, 0)?;
        let mut attributes = Attributes::read(env, attributes)?;
        if attributes.name.as_ref().is_none_or(Vec::is_empty) && attributes.root == 0 {
            return Err(OBJECT_PATH_SYNTAX_BAD.into());
        }
        attributes.attributes |= OBJ_OPENIF | OBJ_CASE_INSENSITIVE;
        let class = match class {
            0 => Vec::new(),
            class => bytes(&read_unicode_string(env, class)?),
        };
        let mut request = CreateKeyRequest {
            access,
            options,
            ..Default::default()
        };
        let objattr = attributes.wire();
        let (reply, _) = server::request(env, &mut request, &[&objattr, &class], &mut [])?;
        let created = match reply.header.error {
            status::SUCCESS => REG_CREATED_NEW_KEY,
            OBJECT_NAME_EXISTS => REG_OPENED_EXISTING_KEY,
            error => return Err(error.into()),
        };
        write_u32(env, key, reply.hkey)?;
        if disposition != 0 {
            write_u32(env, disposition, created)?;
        }
        Ok(())
    })())
}

/// NtQueryValueKey(key, *name, class, *info, length, *result_length)
pub fn query_value(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[key, name, class, info, length, result_length] = args else {
        unreachable!()
    };
    status::finish((|| {
        let name = read_unicode_string(env, name)?;
        if 2 * name.len() > MAX_VALUE_LENGTH {
            return Err(status::OBJECT_NAME_NOT_FOUND.into());
        }
        let name = bytes(&name);
        // Where the name and the data go, and how much comes before them.
        let (minimum, name_at, data_at) = match class {
            KEY_VALUE_BASIC => (12, Some(12), None),
            KEY_VALUE_FULL => (20, Some(20), Some(20 + name.len() as u32)),
            KEY_VALUE_PARTIAL => (12, None, Some(12)),
            KEY_VALUE_PARTIAL_ALIGN64 => (8, None, Some(8)),
            _ => return Err(status::INVALID_PARAMETER.into()),
        };
        let fixed = minimum
            + if name_at.is_some() {
                name.len() as u32
            } else {
                0
            };
        if let Some(at) = name_at.filter(|_| length > minimum) {
            let count = name.len().min((length - minimum) as usize);
            write_bytes(env, info + at, &name[..count])?;
        }
        let mut data = vec![0u8; length.saturating_sub(fixed) as usize];
        if data_at.is_none() {
            data.clear();
        }
        let mut request = GetKeyValueRequest {
            hkey: key,
            ..Default::default()
        };
        let (reply, size) = server::request(env, &mut request, &[&name], &mut data)?;
        status::check(reply.header.error)?;
        if let Some(at) = data_at {
            write_bytes(env, info + at, &data[..size])?;
        }
        let kind = reply.r#type as u32;
        let header: Vec<u32> = match class {
            KEY_VALUE_BASIC => vec![0, kind, name.len() as u32],
            KEY_VALUE_FULL => vec![
                0,
                kind,
                20 + name.len() as u32,
                reply.total,
                name.len() as u32,
            ],
            KEY_VALUE_PARTIAL => vec![0, kind, reply.total],
            _ => vec![kind, reply.total],
        };
        let header: Vec<u8> = header
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        write_bytes(env, info, &header[..header.len().min(length as usize)])?;
        let needed = fixed
            + if class == KEY_VALUE_BASIC {
                0
            } else {
                reply.total
            };
        write_u32(env, result_length, needed)?;
        if length < minimum {
            Err(BUFFER_TOO_SMALL.into())
        } else if length < needed {
            Err(BUFFER_OVERFLOW.into())
        } else {
            Ok(())
        }
    })())
}

/// NtSetValueKey(key, *name, index, type, *data, size)
pub fn set_value(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[key, name, _, kind, data, size] = args else {
        unreachable!()
    };
    status::finish((|| {
        let name = read_unicode_string(env, name)?;
        if 2 * name.len() > MAX_VALUE_LENGTH {
            return Err(status::INVALID_PARAMETER.into());
        }
        let name = bytes(&name);
        let mut value = vec![0u8; size as usize];
        env.cpu
            .read(data, &mut value)
            .map_err(|_| status::ACCESS_VIOLATION)?;
        let mut request = SetKeyValueRequest {
            hkey: key,
            r#type: kind as i32,
            namelen: name.len() as u32,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&name, &value], &mut [])?;
        status::check(reply.header.error)
    })())
}

fn bytes(text: &[u16]) -> Vec<u8> {
    text.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}
