//! Access tokens, after Wine's `unix/security.c`: the server holds them.

use adv_machine::{Env, Outcome};

use crate::object::{write_bytes, write_u32};
use crate::protocol::GetTokenSidRequest;
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
        if class != TOKEN_USER {
            return Err(Error::Unsupported(format!(
                "token information class {class}"
            )));
        }
        if result_length != 0 {
            write_u32(env, result_length, 0)?;
        }
        let mut sid = vec![0u8; length.saturating_sub(TOKEN_USER_SIZE) as usize];
        let mut request = GetTokenSidRequest {
            handle: token,
            which_sid: class,
            ..Default::default()
        };
        let (reply, size) = server::request(env, &mut request, &[], &mut sid)?;
        if result_length != 0 {
            write_u32(env, result_length, reply.sid_len + TOKEN_USER_SIZE)?;
        }
        status::check(reply.header.error)?;
        let at = info + TOKEN_USER_SIZE;
        write_bytes(env, at, &sid[..size])?;
        write_u32(env, info, at)?;
        write_u32(env, info + 4, 0)?;
        Ok(())
    })())
}
