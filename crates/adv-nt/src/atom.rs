//! Global atoms, after Wine's `unix/sync.c`: the server holds the table;
//! integral atoms (`#123` or a value below 0xc000) never reach it.

use adv_machine::{Env, Outcome};

use crate::object::{write_bytes, write_u32};
use crate::protocol::{
    AddAtomRequest, DeleteAtomRequest, FindAtomRequest, GetAtomInformationRequest,
};
use crate::server;
use crate::status::{self, Error, Status};

const MAXINTATOM: u32 = 0xc000;
const MAX_ATOM_LEN: usize = 255;
const INVALID_INFO_CLASS: Status = 0xc000_0003;
const ATOM_BASIC_INFORMATION: u32 = 0;
/// The size of ATOM_BASIC_INFORMATION, whose name starts at `NAME`.
const BASIC_SIZE: u32 = 8;
const NAME: u32 = 6;

/// The integral atom `name` (a pointer below 0x10000, or `#` and digits)
/// stands for, or `None` for a string atom.
fn integral(env: &Env<'_>, name: u32, length: u32) -> Result<Option<u16>, Error> {
    if name >> 16 == 0 {
        return match name {
            1..MAXINTATOM => Ok(Some(name as u16)),
            _ => Err(status::INVALID_PARAMETER.into()),
        };
    }
    if length == 0 {
        return Err(status::OBJECT_NAME_INVALID.into());
    }
    let mut bytes = vec![0; length as usize / 2 * 2];
    env.cpu
        .read(name, &mut bytes)
        .map_err(|_| status::ACCESS_VIOLATION)?;
    let text: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();
    if let Some(digits) = text.strip_prefix(&[u16::from(b'#')])
        && !digits.is_empty()
        && digits
            .iter()
            .all(|&c| (u16::from(b'0')..=u16::from(b'9')).contains(&c))
    {
        let value = digits.iter().fold(0u32, |value, &c| {
            value
                .saturating_mul(10)
                .saturating_add(u32::from(c - u16::from(b'0')))
        });
        return match value {
            1..MAXINTATOM => Ok(Some(value as u16)),
            _ => Err(status::INVALID_PARAMETER.into()),
        };
    }
    if text.len() > MAX_ATOM_LEN {
        return Err(status::INVALID_PARAMETER.into());
    }
    Ok(None)
}

/// NtAddAtom(*name, length, *atom)
pub fn add(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    lookup(env, args, |env, name| {
        let mut request = AddAtomRequest::default();
        let (reply, _) = server::request(env, &mut request, &[name], &mut [])?;
        status::check(reply.header.error)?;
        Ok(reply.atom)
    })
}

/// NtFindAtom(*name, length, *atom)
pub fn find(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    lookup(env, args, |env, name| {
        let mut request = FindAtomRequest::default();
        let (reply, _) = server::request(env, &mut request, &[name], &mut [])?;
        status::check(reply.header.error)?;
        Ok(reply.atom)
    })
}

/// Stores the atom of `name`, integral or as `server` finds it.
fn lookup(
    env: &mut Env<'_>,
    args: &[u32],
    server: impl FnOnce(&mut Env<'_>, &[u8]) -> Result<u32, Error>,
) -> Outcome {
    let &[name, length, atom] = args else {
        unreachable!()
    };
    status::finish((|| {
        let value = match integral(env, name, length)? {
            Some(value) => value,
            None => {
                let mut bytes = vec![0; length as usize];
                env.cpu
                    .read(name, &mut bytes)
                    .map_err(|_| status::ACCESS_VIOLATION)?;
                server(env, &bytes)? as u16
            }
        };
        Ok(write_bytes(env, atom, &value.to_le_bytes())?)
    })())
}

/// NtDeleteAtom(atom)
pub fn delete(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let atom = args[0] & 0xffff;
    if atom == 0 {
        return Outcome::Return(status::INVALID_HANDLE);
    }
    if atom < MAXINTATOM {
        return Outcome::Return(status::SUCCESS);
    }
    let mut request = DeleteAtomRequest {
        atom,
        ..Default::default()
    };
    status::finish(
        server::request(env, &mut request, &[], &mut [])
            .and_then(|(reply, _)| status::check(reply.header.error)),
    )
}

/// NtQueryInformationAtom(atom, class, *info, size, *returned)
pub fn query(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[atom, class, info, size, returned] = args else {
        unreachable!()
    };
    let atom = atom & 0xffff;
    if class != ATOM_BASIC_INFORMATION {
        return Outcome::Return(INVALID_INFO_CLASS);
    }
    if size < BASIC_SIZE {
        return Outcome::Return(status::INVALID_PARAMETER);
    }
    // ATOM_BASIC_INFORMATION: ReferenceCount, Pinned, NameLength, Name.
    let capacity = size - BASIC_SIZE;
    let result = (|| {
        let (count, pinned, length, reported, result) = if atom < MAXINTATOM {
            if atom == 0 {
                return Err(Error::from(status::INVALID_PARAMETER));
            }
            let mut length = 0;
            if let Some(fits) = (capacity as usize / 2).checked_sub(1) {
                let mut name: Vec<u16> = format!("#{atom}").encode_utf16().collect();
                name.truncate(fits);
                name.push(0);
                let bytes: Vec<u8> = name.iter().flat_map(|unit| unit.to_le_bytes()).collect();
                write_bytes(env, info + NAME, &bytes)?;
                length = 2 * (name.len() as u32 - 1);
            }
            let result = if capacity == 0 {
                status::BUFFER_TOO_SMALL
            } else {
                status::SUCCESS
            };
            (1, 1, length, capacity, result)
        } else {
            let mut request = GetAtomInformationRequest {
                atom,
                ..Default::default()
            };
            let mut name = vec![0u8; capacity as usize];
            let (reply, size) = server::request(env, &mut request, &[], &mut name)?;
            status::check(reply.header.error)?;
            if size == 0 {
                (
                    reply.count,
                    reply.pinned,
                    reply.total,
                    reply.total,
                    status::BUFFER_TOO_SMALL,
                )
            } else {
                name.truncate(size);
                name.extend_from_slice(&[0, 0]);
                write_bytes(env, info + NAME, &name)?;
                (
                    reply.count,
                    reply.pinned,
                    size as u32,
                    size as u32,
                    status::SUCCESS,
                )
            }
        };
        let header: Vec<u8> = [count as u16, pinned as u16, length as u16]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        write_bytes(env, info, &header)?;
        if returned != 0 {
            write_u32(env, returned, BASIC_SIZE + reported)?;
        }
        Ok(result)
    })();
    match result {
        Ok(result) => Outcome::Return(result),
        Err(error) => status::finish(Err(error)),
    }
}
