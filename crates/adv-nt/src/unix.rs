//! The Unix library calls ntdll.dll makes into ntdll.so, numbered as in
//! Wine's `unix_funcs` table (`dlls/ntdll/unixlib.h`).

use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};

use crate::protocol::FIXED_SIZE;
use crate::server::Link;

const WINE_SERVER_CALL: u32 = 3;

/// `struct __server_request_info` (i386): the fixed part, then the data
/// count, the reply data pointer and up to five request data vectors.
const DATA_COUNT: u32 = FIXED_SIZE as u32;
const REPLY_DATA: u32 = DATA_COUNT + 4;
const DATA: u32 = REPLY_DATA + 4;
const MAX_DATA: u32 = 5;
const REPLY_SIZE: usize = 8;

/// __wine_unix_call_dispatcher(handle, code, args)
pub fn call(env: &mut Env<'_>, code: u32, args: u32) -> Outcome {
    match code {
        WINE_SERVER_CALL => server_call(env, args),
        _ => Outcome::Fail(format!("unix call {code} is not implemented")),
    }
}

/// Passes a request ntdll.dll prepared to the server.
fn server_call(env: &mut Env<'_>, request: u32) -> Outcome {
    match forward(env, request) {
        Ok(status) => Outcome::Return(status),
        Err(error) => Outcome::Fail(error),
    }
}

fn forward(env: &mut Env<'_>, request: u32) -> Result<u32, String> {
    let fault = |error: adv_cpu::backend::MemFault| error.to_string();
    let mut fixed = [0u8; FIXED_SIZE];
    env.cpu.read(request, &mut fixed).map_err(fault)?;
    let count = env.cpu.read_u32(request + DATA_COUNT).map_err(fault)?;
    if count > MAX_DATA {
        return Err(format!("server request with {count} data vectors"));
    }
    let mut data = Vec::with_capacity(count as usize);
    for index in 0..count {
        let vector = request + DATA + 8 * index;
        let pointer = env.cpu.read_u32(vector).map_err(fault)?;
        let size = env.cpu.read_u32(vector + 4).map_err(fault)?;
        let mut bytes = vec![0; size as usize];
        env.cpu.read(pointer, &mut bytes).map_err(fault)?;
        data.push(bytes);
    }
    let reply_pointer = env.cpu.read_u32(request + REPLY_DATA).map_err(fault)?;
    let reply_size = u32::from_le_bytes(fixed[REPLY_SIZE..REPLY_SIZE + 4].try_into().unwrap());
    let mut reply = vec![0; reply_size as usize];

    let parts: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
    let link = env
        .ext
        .try_get_mut::<Link>()
        .ok_or("no server connection")?;
    let size = link
        .call_raw(&mut fixed, &parts, &mut reply)
        .map_err(|error| format!("server call failed: {error}"))?;
    env.cpu.write(request, &fixed).map_err(fault)?;
    env.cpu
        .write(reply_pointer, &reply[..size])
        .map_err(fault)?;
    Ok(u32::from_le_bytes(fixed[..4].try_into().unwrap()))
}
