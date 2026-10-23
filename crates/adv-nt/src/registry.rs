//! Registry system calls, after Wine's `unix/registry.c`: the server holds
//! the registry; these calls pass the request on and lay out the reply.

use adv_machine::{Env, Outcome};

use crate::object::{Attributes, read_unicode_string, write_bytes, write_u32};
use crate::protocol::{
    CreateKeyRequest, DeleteKeyRequest, DeleteKeyValueRequest, EnumKeyRequest, EnumKeyValueRequest,
    GetKeyValueRequest, OpenKeyRequest, SetKeyValueRequest,
};
use crate::server;
use crate::status::{self, Error, Status};

const OBJ_OPENIF: u32 = 0x80;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;
const OBJECT_PATH_SYNTAX_BAD: Status = 0xc000_003b;
const BUFFER_OVERFLOW: Status = 0x8000_0005;
const BUFFER_TOO_SMALL: Status = 0xc000_0023;
const NO_MORE_ENTRIES: Status = 0x8000_001a;
const KEY_READ: u32 = 0x2_0019;
const KEY_BASIC_INFORMATION: i32 = 0;
const KEY_NODE_INFORMATION: i32 = 1;
const KEY_FULL_INFORMATION: i32 = 2;
const KEY_NAME_INFORMATION: i32 = 3;
const KEY_CACHED_INFORMATION: i32 = 4;
/// The longest key name, in bytes.
const MAX_KEY_NAME: usize = 255 * 2;
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
    let opened = open_named(
        env,
        attributes.root,
        access,
        attributes.attributes,
        &attributes.name_bytes(),
    )?;
    write_u32(env, key, opened)?;
    Ok(())
}

fn open_named(
    env: &mut Env<'_>,
    parent: u32,
    access: u32,
    attributes: u32,
    name: &[u8],
) -> Result<u32, Error> {
    let mut request = OpenKeyRequest {
        parent,
        access,
        attributes: attributes | OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let (reply, _) = server::request(env, &mut request, &[name], &mut [])?;
    status::check(reply.header.error)?;
    Ok(reply.hkey)
}

/// Opens key `name` under `parent`, or a full `\Registry` path, for the
/// platform's own reads.
pub fn open_for_reading(env: &mut Env<'_>, parent: u32, name: &str) -> Result<u32, Error> {
    let name: Vec<u16> = name.encode_utf16().collect();
    open_named(env, parent, KEY_READ, 0, &bytes(&name))
}

/// The name of subkey `index` of `key`, or `None` past the last one.
pub fn subkey(env: &mut Env<'_>, key: u32, index: u32) -> Result<Option<Vec<u16>>, Error> {
    let mut request = EnumKeyRequest {
        hkey: key,
        index: index as i32,
        info_class: KEY_BASIC_INFORMATION,
        ..Default::default()
    };
    let mut name = vec![0u8; MAX_KEY_NAME];
    let (reply, size) = server::request(env, &mut request, &[], &mut name)?;
    match reply.header.error {
        NO_MORE_ENTRIES => return Ok(None),
        error => status::check(error)?,
    }
    let size = size.min(reply.namelen as usize);
    Ok(Some(
        name[..size]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .collect(),
    ))
}

/// The type and data of value `name` of `key`, if it exists and holds at
/// most `limit` bytes.
pub fn value(
    env: &mut Env<'_>,
    key: u32,
    name: &str,
    limit: usize,
) -> Result<Option<(u32, Vec<u8>)>, Error> {
    let name: Vec<u16> = name.encode_utf16().collect();
    let mut request = GetKeyValueRequest {
        hkey: key,
        ..Default::default()
    };
    let mut data = vec![0u8; limit];
    let (reply, size) = server::request(env, &mut request, &[&bytes(&name)], &mut data)?;
    if reply.header.error != status::SUCCESS || reply.total as usize > limit {
        return Ok(None);
    }
    data.truncate(size);
    Ok(Some((reply.r#type as u32, data)))
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
            status::OBJECT_NAME_EXISTS => REG_OPENED_EXISTING_KEY,
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

/// NtDeleteKey(key)
pub fn delete(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let mut request = DeleteKeyRequest {
        hkey: args[0],
        ..Default::default()
    };
    status::finish(
        server::request(env, &mut request, &[], &mut [])
            .and_then(|(reply, _)| status::check(reply.header.error)),
    )
}

/// NtDeleteValueKey(key, *name)
pub fn delete_value(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[key, name] = args else { unreachable!() };
    status::finish((|| {
        let name = read_unicode_string(env, name)?;
        if 2 * name.len() > MAX_VALUE_LENGTH {
            return Err(status::OBJECT_NAME_NOT_FOUND.into());
        }
        let mut request = DeleteKeyValueRequest {
            hkey: key,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&bytes(&name)], &mut [])?;
        status::check(reply.header.error)
    })())
}

/// NtEnumerateKey(key, index, class, *info, length, *result_length)
pub fn enumerate(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[key, index, class, info, length, result_length] = args else {
        unreachable!()
    };
    // Index -1 queries the key itself.
    if index == u32::MAX {
        return Outcome::Return(NO_MORE_ENTRIES);
    }
    status::finish(key_information(
        env,
        key,
        index as i32,
        class as i32,
        info,
        length,
        result_length,
    ))
}

/// NtQueryKey(key, class, *info, length, *result_length)
pub fn query(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[key, class, info, length, result_length] = args else {
        unreachable!()
    };
    status::finish(key_information(
        env,
        key,
        -1,
        class as i32,
        info,
        length,
        result_length,
    ))
}

/// The KEY_*_INFORMATION of subkey `index` of `key`, or of `key` itself
/// for -1, as Wine's `enumerate_key` lays it out.
fn key_information(
    env: &mut Env<'_>,
    key: u32,
    index: i32,
    class: i32,
    info: u32,
    length: u32,
    result_length: u32,
) -> Result<(), Error> {
    let fixed: u32 = match class {
        KEY_BASIC_INFORMATION => 16,
        KEY_FULL_INFORMATION => 44,
        KEY_NODE_INFORMATION => 24,
        KEY_NAME_INFORMATION => 4,
        KEY_CACHED_INFORMATION => 40,
        _ => return Err(status::INVALID_PARAMETER.into()),
    };
    let mut request = EnumKeyRequest {
        hkey: key,
        index,
        info_class: class,
        ..Default::default()
    };
    let mut data = vec![0u8; length.saturating_sub(fixed) as usize];
    let (reply, size) = server::request(env, &mut request, &[], &mut data)?;
    status::check(reply.header.error)?;
    write_bytes(env, info + fixed, &data[..size])?;
    let (low, high) = (reply.modif as u32, (reply.modif >> 32) as u32);
    let size = size as u32;
    let header: Vec<u32> = match class {
        KEY_BASIC_INFORMATION => vec![low, high, 0, reply.namelen],
        KEY_FULL_INFORMATION => vec![
            low,
            high,
            0,
            if size != 0 { fixed } else { u32::MAX },
            size,
            reply.subkeys as u32,
            reply.max_subkey as u32,
            reply.max_class as u32,
            reply.values as u32,
            reply.max_value as u32,
            reply.max_data as u32,
        ],
        KEY_NODE_INFORMATION => {
            let (class_offset, class_length) = if reply.namelen < size {
                (fixed + reply.namelen, size - reply.namelen)
            } else {
                (u32::MAX, 0)
            };
            vec![low, high, 0, class_offset, class_length, reply.namelen]
        }
        KEY_NAME_INFORMATION => vec![reply.namelen],
        _ => vec![
            low,
            high,
            0,
            reply.subkeys as u32,
            reply.max_subkey as u32,
            reply.values as u32,
            reply.max_value as u32,
            reply.max_data as u32,
            reply.namelen,
        ],
    };
    let header: Vec<u8> = header
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    write_bytes(env, info, &header[..header.len().min(length as usize)])?;
    let needed = fixed + reply.total;
    write_u32(env, result_length, needed)?;
    if length < fixed {
        Err(BUFFER_TOO_SMALL.into())
    } else if length < needed {
        Err(BUFFER_OVERFLOW.into())
    } else {
        Ok(())
    }
}

/// NtEnumerateValueKey(key, index, class, *info, length, *result_length)
pub fn enumerate_value(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[key, index, class, info, length, result_length] = args else {
        unreachable!()
    };
    status::finish((|| {
        let fixed: u32 = match class {
            KEY_VALUE_BASIC => 12,
            KEY_VALUE_FULL => 20,
            KEY_VALUE_PARTIAL => 12,
            _ => return Err(status::INVALID_PARAMETER.into()),
        };
        let mut request = EnumKeyValueRequest {
            hkey: key,
            index: index as i32,
            info_class: class as i32,
            ..Default::default()
        };
        let mut data = vec![0u8; length.saturating_sub(fixed) as usize];
        let (reply, size) = server::request(env, &mut request, &[], &mut data)?;
        status::check(reply.header.error)?;
        write_bytes(env, info + fixed, &data[..size])?;
        let kind = reply.r#type as u32;
        let data_length = size as u32 - reply.namelen.min(size as u32);
        let header: Vec<u32> = match class {
            KEY_VALUE_BASIC => vec![0, kind, reply.namelen],
            KEY_VALUE_FULL => vec![0, kind, fixed + reply.namelen, data_length, reply.namelen],
            _ => vec![0, kind, data_length],
        };
        let header: Vec<u8> = header
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        write_bytes(env, info, &header[..header.len().min(length as usize)])?;
        let needed = fixed + reply.total;
        write_u32(env, result_length, needed)?;
        if length < needed {
            Err(BUFFER_OVERFLOW.into())
        } else {
            Ok(())
        }
    })())
}

fn bytes(text: &[u16]) -> Vec<u8> {
    text.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}
