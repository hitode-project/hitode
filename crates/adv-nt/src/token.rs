//! Access tokens, after Wine's `unix/security.c`: the server holds them.

use adv_machine::{Env, Outcome};

use crate::object::{write_bytes, write_u32};
use crate::protocol::{AllocateLocallyUniqueIdRequest, GetTokenSidRequest};
use crate::server;
use crate::status::{self, Error};

const TOKEN_USER: u32 = 1;
/// TOKEN_USER: the SID pointer and its attributes; the SID follows.
const TOKEN_USER_SIZE: u32 = 8;

/// NtQueryInformationToken(token, class, *info, length, *result_length)
pub fn query(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[token, class, info, length, result_length] = args else {
        unreachable!()
    };
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
