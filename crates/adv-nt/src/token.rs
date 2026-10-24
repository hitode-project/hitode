//! Access tokens, after Wine's `unix/security.c`: the server holds them.

use adv_machine::{Env, Outcome};

use crate::object::{read_bytes, read_u32, write_bytes, write_u32};
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
    if class == 6 {
        return status::finish((|| {
            let mut request = crate::protocol::GetTokenDefaultDaclRequest {
                handle: token,
                ..Default::default()
            };
            let mut acl = vec![0; length.saturating_sub(4) as usize];
            let (reply, size) = server::request(env, &mut request, &[], &mut acl)?;
            if result_length != 0 {
                write_u32(env, result_length, 4 + reply.acl_len)?;
            }
            status::check(reply.header.error)?;
            write_bytes(env, info + 4, &acl[..size])?;
            Ok(write_u32(
                env,
                info,
                if reply.acl_len == 0 { 0 } else { info + 4 },
            )?)
        })());
    }
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
    open_process_ex(env, &[args[0], args[1], 0, args[2]])
}

/// NtOpenProcessTokenEx(process, access, attributes, *handle)
pub fn open_process_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    open_token(env, args[0], args[1], args[2], 0, args[3])
}

/// NtOpenThreadToken(thread, access, self, *handle)
pub fn open_thread(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    open_thread_ex(env, &[args[0], args[1], args[2], 0, args[3]])
}

/// NtOpenThreadTokenEx(thread, access, self, attributes, *handle)
pub fn open_thread_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    open_token(
        env,
        args[0],
        args[1],
        args[3],
        1 | if args[2] as u8 != 0 { 2 } else { 0 },
        args[4],
    )
}

fn open_token(
    env: &mut Env<'_>,
    handle: u32,
    access: u32,
    attributes: u32,
    flags: u32,
    output: u32,
) -> Outcome {
    status::finish((|| {
        write_u32(env, output, 0)?;
        let mut request = crate::protocol::OpenTokenRequest {
            handle,
            access,
            attributes,
            flags,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        Ok(write_u32(env, output, reply.token)?)
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

/// Reads the variable-sized SID at a guest address.
fn sid(env: &Env<'_>, address: u32) -> Result<Vec<u8>, Error> {
    let count = (read_u32(env, address)? >> 8) & 0xff;
    Ok(read_bytes(env, address, 8 + 4 * count)?)
}

/// NtCreateToken, with groups, privileges and the default DACL sent to the server.
pub fn create(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attr = if args[2] == 0 {
            Vec::new()
        } else {
            crate::object::Attributes::read(env, args[2])?.wire()
        };
        let qos = read_u32(env, args[2] + 20)?;
        let level = if qos == 0 {
            0
        } else {
            read_u32(env, qos + 4)? as i32
        };
        let count = read_u32(env, args[7])?;
        let mut primary_group = -1;
        let mut attributes = Vec::new();
        let mut sids = Vec::new();
        for i in 0..count {
            let entry = args[7] + 4 + 8 * i;
            let value = sid(env, read_u32(env, entry)?)?;
            if value == sid(env, read_u32(env, args[10])?)? {
                primary_group = i as i32;
            }
            attributes.extend_from_slice(&read_u32(env, entry + 4)?.to_le_bytes());
            sids.extend_from_slice(&value);
        }
        if primary_group == -1 {
            return Err(0xc000_005b.into());
        }
        attributes.extend_from_slice(&sids);
        let user = sid(env, read_u32(env, args[6])?)?;
        let priv_count = read_u32(env, args[8])?;
        let privileges = read_bytes(env, args[8] + 4, priv_count * 12)?;
        let acl = if args[11] == 0 {
            0
        } else {
            read_u32(env, args[11])?
        };
        let dacl = if acl == 0 {
            Vec::new()
        } else {
            read_bytes(env, acl, read_u32(env, acl)? >> 16)?
        };
        let mut request = crate::protocol::CreateTokenRequest {
            token_id: read_bytes(env, args[4], 8)?.try_into().unwrap(),
            access: args[1],
            primary: i32::from(args[3] == 1),
            impersonation_level: level,
            expire: i64::from_le_bytes(read_bytes(env, args[5], 8)?.try_into().unwrap()),
            group_count: count as i32,
            primary_group,
            priv_count: priv_count as i32,
            ..Default::default()
        };
        let (reply, _) = server::request(
            env,
            &mut request,
            &[&attr, &user, &attributes, &privileges, &dacl],
            &mut [],
        )?;
        status::check(reply.header.error)?;
        Ok(write_u32(env, args[0], reply.token)?)
    })())
}

/// NtSetInformationToken(token, class, info, length)
pub fn set(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| match args[1] {
        6 | 12 => {
            if args[3] < 4 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            if args[2] == 0 {
                return Err(status::ACCESS_VIOLATION.into());
            }
            if args[1] == 12 {
                return Ok(());
            }
            let acl = read_u32(env, args[2])?;
            let data = if acl == 0 {
                Vec::new()
            } else {
                read_bytes(env, acl, read_u32(env, acl)? >> 16)?
            };
            let mut request = crate::protocol::SetTokenDefaultDaclRequest {
                handle: args[0],
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[&data], &mut [])?;
            status::check(reply.header.error)
        }
        25 => Ok(()),
        _ => Err(status::NOT_IMPLEMENTED.into()),
    })())
}

/// NtCreateLowBoxToken: Wine returns a null handle and success.
pub fn create_low_box(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish(write_u32(env, args[0], 0).map_err(Into::into))
}

/// NtAdjustGroupsToken: Wine's stub.
pub fn adjust_groups(_: &mut Env<'_>, _: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// NtAdjustPrivilegesToken(token, disable, privileges, length, previous, retlen)
pub fn adjust_privileges(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let disable = args[1] as u8;
        let data = if disable != 0 {
            Vec::new()
        } else {
            read_bytes(env, args[2] + 4, read_u32(env, args[2])? * 12)?
        };
        let mut previous = vec![
            0;
            if args[4] != 0 {
                args[3].saturating_sub(4) as usize
            } else {
                0
            }
        ];
        let mut request = crate::protocol::AdjustTokenPrivilegesRequest {
            handle: args[0],
            disable_all: i32::from(disable),
            get_modified_state: i32::from(args[4] != 0),
            ..Default::default()
        };
        let (reply, size) = server::request(env, &mut request, &[&data], &mut previous)?;
        if args[4] != 0 {
            write_bytes(env, args[4] + 4, &previous[..size])?;
            if args[5] != 0 {
                write_u32(env, args[5], reply.len + 4)?;
            }
            write_u32(env, args[4], reply.len / 12)?;
        }
        status::check(reply.header.error)
    })())
}

/// NtFilterToken(token, flags, disable_sids, privileges, restrict_sids, new_token)
pub fn filter(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let privileges = if args[3] == 0 {
            Vec::new()
        } else {
            read_bytes(env, args[3] + 4, read_u32(env, args[3])? * 12)?
        };
        let mut sids = Vec::new();
        if args[2] != 0 {
            for i in 0..read_u32(env, args[2])? {
                sids.extend_from_slice(&sid(env, read_u32(env, args[2] + 4 + 8 * i)?)?);
            }
        }
        let mut request = crate::protocol::FilterTokenRequest {
            handle: args[0],
            flags: args[1],
            privileges_size: privileges.len() as u32,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&privileges, &sids], &mut [])?;
        status::check(reply.header.error)?;
        Ok(write_u32(env, args[5], reply.new_handle)?)
    })())
}

/// NtPrivilegeCheck(token, privileges, result)
pub fn privilege_check(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let data = read_bytes(env, args[1] + 8, read_u32(env, args[1])? * 12)?;
        let mut output = vec![0; data.len()];
        let mut request = crate::protocol::CheckTokenPrivilegesRequest {
            handle: args[0],
            all_required: i32::from(read_u32(env, args[1] + 4)? & 1 != 0),
            ..Default::default()
        };
        let (reply, size) = server::request(env, &mut request, &[&data], &mut output)?;
        write_bytes(env, args[1] + 8, &output[..size])?;
        status::check(reply.header.error)?;
        Ok(write_bytes(
            env,
            args[2],
            &[u8::from(reply.has_privileges != 0)],
        )?)
    })())
}

/// NtImpersonateAnonymousToken: Wine's stub.
pub fn impersonate_anonymous(_: &mut Env<'_>, _: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// NtCompareTokens: Wine's stub.
pub fn compare(_: &mut Env<'_>, _: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// NtAllocateUuids: Wine leaves all outputs untouched.
pub fn allocate_uuids(_: &mut Env<'_>, _: &[u32]) -> Outcome {
    Outcome::Return(status::SUCCESS)
}
