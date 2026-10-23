//! Access tokens, after Wine's `unix/security.c`: the server holds them.

use adv_machine::{Env, Outcome};

use crate::object::{write_bytes, write_u32};
use crate::protocol::{AllocateLocallyUniqueIdRequest, GetTokenGroupsRequest, GetTokenSidRequest};
use crate::server;
use crate::status::{self, Error};

const TOKEN_USER: u32 = 1;
const TOKEN_GROUPS: u32 = 2;
const TOKEN_LOGON_SID: u32 = 28;
const SE_GROUP_LOGON_ID: u32 = 0xc000_0000;
/// TOKEN_USER: the SID pointer and its attributes; the SID follows.
const TOKEN_USER_SIZE: u32 = 8;

/// NtQueryInformationToken(token, class, *info, length, *result_length)
pub fn query(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[token, class, info, length, result_length] = args else {
        unreachable!()
    };
    if class == TOKEN_GROUPS || class == TOKEN_LOGON_SID {
        return status::finish(groups(env, token, class, info, length, result_length));
    }
    status::finish((|| {
        if class != TOKEN_USER && class != 4 {
            return Err(Error::Unsupported(format!(
                "token information class {class}"
            )));
        }
        if result_length != 0 {
            write_u32(env, result_length, 0)?;
        }
        let header = if class == TOKEN_USER {
            TOKEN_USER_SIZE
        } else {
            4
        };
        let mut sid = vec![0u8; length.saturating_sub(header) as usize];
        let mut request = GetTokenSidRequest {
            handle: token,
            which_sid: class,
            ..Default::default()
        };
        let (reply, size) = server::request(env, &mut request, &[], &mut sid)?;
        if result_length != 0 {
            write_u32(env, result_length, reply.sid_len + header)?;
        }
        status::check(reply.header.error)?;
        let at = info + header;
        write_bytes(env, at, &sid[..size])?;
        write_u32(env, info, at)?;
        if class == TOKEN_USER {
            write_u32(env, info + 4, 0)?;
        }
        Ok(())
    })())
}

/// TOKEN_GROUPS: the count, each group's SID pointer and attributes, then
/// the SIDs, as the server lists them.
fn groups(
    env: &mut Env<'_>,
    token: u32,
    class: u32,
    info: u32,
    length: u32,
    result_length: u32,
) -> Result<(), Error> {
    let mut request = GetTokenGroupsRequest {
        handle: token,
        attr_mask: if class == TOKEN_LOGON_SID {
            SE_GROUP_LOGON_ID
        } else {
            0
        },
        ..Default::default()
    };
    let mut buffer = vec![0u8; length as usize];
    let (reply, _) = server::request(env, &mut request, &[], &mut buffer)?;
    let count = reply.attr_len / 4;
    let needed = 4 + 8 * count + reply.sid_len;
    let mut error = reply.header.error;
    if error == status::SUCCESS && needed > length {
        error = status::BUFFER_TOO_SMALL;
    }
    if result_length != 0 {
        let size = if error == status::SUCCESS || error == status::BUFFER_TOO_SMALL {
            needed
        } else {
            0
        };
        write_u32(env, result_length, size)?;
    }
    status::check(error)?;
    let sids = &buffer[reply.attr_len as usize..(reply.attr_len + reply.sid_len) as usize];
    let mut words = vec![count];
    let mut at = info + 4 + 8 * count;
    let mut offset = 0;
    for index in 0..count as usize {
        let attributes = &buffer[4 * index..4 * index + 4];
        words.extend([at, u32::from_le_bytes(attributes.try_into().unwrap())]);
        // A SID's size: its header, then four bytes per sub-authority.
        let size = 8 + 4 * u32::from(sids[offset + 1]);
        at += size;
        offset += size as usize;
    }
    let mut bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    bytes.extend_from_slice(sids);
    Ok(write_bytes(env, info, &bytes)?)
}

/// NtAllocateLocallyUniqueId(*luid)
pub fn allocate_locally_unique_id(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] == 0 {
        return Outcome::Return(status::ACCESS_VIOLATION);
    }
    status::finish((|| {
        let mut request = AllocateLocallyUniqueIdRequest::default();
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        Ok(write_bytes(env, args[0], &reply.luid)?)
    })())
}

/// NtOpenProcessToken(process, access, *handle)
pub fn open_process(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[process, access, handle] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        let mut request = crate::protocol::OpenTokenRequest {
            handle: process,
            access,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, handle, reply.token)?;
        Ok(())
    })())
}

/// NtOpenThreadToken(thread, access, self, *handle)
pub fn open_thread(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[3], 0)?;
        let mut request = crate::protocol::OpenTokenRequest {
            handle: args[0],
            access: args[1],
            flags: 1 | if args[2] as u8 != 0 { 2 } else { 0 },
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, args[3], reply.token)?;
        Ok(())
    })())
}

/// NtDuplicateToken(token, access, *attributes, effective_only, type, *handle)
pub fn duplicate(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[5], 0)?;
        let attr = if args[2] == 0 {
            Vec::new()
        } else {
            crate::object::Attributes::read(env, args[2])?.wire()
        };
        let qos = if args[2] == 0 {
            0
        } else {
            crate::object::read_u32(env, args[2] + 20)?
        };
        let level = if qos == 0 {
            0
        } else {
            crate::object::read_u32(env, qos + 4)? as i32
        };
        let mut request = crate::protocol::DuplicateTokenRequest {
            handle: args[0],
            access: args[1],
            primary: i32::from(args[4] == 1),
            impersonation_level: level,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attr], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, args[5], reply.new_handle)?;
        Ok(())
    })())
}
