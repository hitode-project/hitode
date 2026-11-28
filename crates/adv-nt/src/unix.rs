//! The Unix library calls ntdll.dll makes into ntdll.so, numbered as in
//! Wine's `unix_funcs` table (`dlls/ntdll/unixlib.h`).

use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};

use crate::protocol::{CloseHandleRequest, DupHandleRequest, FIXED_SIZE, Request};
use crate::server::Link;

const WINE_DBG_WRITE: u32 = 2;
const WINE_SERVER_CALL: u32 = 3;
const WINE_SPAWNVP: u32 = 6;

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
        WINE_DBG_WRITE => debug_write(env, args),
        WINE_SERVER_CALL => server_call(env, args),
        WINE_SPAWNVP => spawnvp(env, args),
        7 => crate::system::query_time(env, &[args]),
        _ => Outcome::Fail(format!("unix call {code} is not implemented")),
    }
}

/// Counted Wine `err:` / `fixme:` debug tags (`(level, channel, func) -> count`),
/// shared across a session's processes. Message bodies are never retained.
#[derive(Default)]
pub struct WineLogCounts {
    pub entries: std::collections::BTreeMap<(String, String, String), u64>,
}

fn parse_wine_log_tag(line: &str) -> Option<(&'static str, &str, &str)> {
    let line = line.trim_start();
    // Wine prefixes debug lines with `<tid>:` (e.g. `0024:err:module:import_dll ...`).
    let rest = match line.split_once(':') {
        Some((tid, after))
            if !tid.is_empty() && tid.len() <= 8 && tid.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            after
        }
        _ => line,
    };
    let (level, after) = if let Some(after) = rest.strip_prefix("err:") {
        ("err", after)
    } else {
        ("fixme", rest.strip_prefix("fixme:")?)
    };
    let (channel, after) = after.split_once(':')?;
    let func = after.split_whitespace().next()?.trim_end_matches(':');
    let ok = |s: &str, max: usize| {
        !s.is_empty()
            && s.len() <= max
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    };
    (ok(channel, 32) && ok(func, 64)).then_some((level, channel, func))
}

/// Wine's debug output, which ntdll.so writes to standard error; here it
/// goes to the host log. `struct wine_dbg_write_params { str, len }`.
fn debug_write(env: &mut Env<'_>, params: u32) -> Outcome {
    let text = (|| {
        let pointer = env.cpu.read_u32(params)?;
        let length = env.cpu.read_u32(params + 4)?;
        let mut bytes = vec![0; length as usize];
        env.cpu.read(pointer, &mut bytes)?;
        Ok::<_, adv_cpu::backend::MemFault>(bytes)
    })();
    match text {
        Ok(bytes) => {
            let line = String::from_utf8_lossy(&bytes);
            let trimmed = line.trim_end();
            log::warn!(target: "wine", "{trimmed}");
            if let Some(counts) = env
                .ext
                .try_get::<std::rc::Rc<std::cell::RefCell<WineLogCounts>>>()
                && let Some((level, channel, func)) = parse_wine_log_tag(trimmed)
            {
                let mut counts = counts.borrow_mut();
                let key = (level.to_owned(), channel.to_owned(), func.to_owned());
                if let Some(n) = counts.entries.get_mut(&key) {
                    *n = n.saturating_add(1);
                } else if counts.entries.len() < 64 {
                    counts.entries.insert(key, 1);
                }
            }
            Outcome::Return(bytes.len() as u32)
        }
        Err(error) => Outcome::Fail(format!("debug output: {error}")),
    }
}

/// Web and mail addresses the session's processes asked the host to open,
/// shared by its processes.
#[derive(Default)]
pub struct Links(pub Vec<String>);

/// `__wine_unix_spawnvp(argv, wait)`. Wine's browser and mailer launchers
/// pass the address last, after a host program to try; the host opens the
/// address itself and runs no program. Anything else fails, as a missing
/// program does.
fn spawnvp(env: &mut Env<'_>, params: u32) -> Outcome {
    let Ok(argv) = arguments(env, params) else {
        return Outcome::Return(1);
    };
    let Some(links) = env.ext.try_get::<std::rc::Rc<std::cell::RefCell<Links>>>() else {
        return Outcome::Return(1);
    };
    match argv.last().filter(|url| link(url)) {
        Some(url) => {
            links.borrow_mut().0.push(url.clone());
            Outcome::Return(0)
        }
        None => Outcome::Return(1),
    }
}

/// `struct wine_spawnvp_params { char **argv; int wait; }`.
fn arguments(env: &mut Env<'_>, params: u32) -> Result<Vec<String>, adv_cpu::backend::MemFault> {
    let mut argv = env.cpu.read_u32(params)?;
    let mut list = Vec::new();
    // winebrowser passes a candidate's words and the address.
    while list.len() < 16 {
        let pointer = env.cpu.read_u32(argv)?;
        if pointer == 0 {
            break;
        }
        let word = env.cpu.read_cstr(pointer, 4096)?;
        list.push(String::from_utf8_lossy(&word).into_owned());
        argv += 4;
    }
    Ok(list)
}

/// A web or mail address, which a browser or mailer can open.
fn link(url: &str) -> bool {
    let scheme = url.split_once(':').map_or("", |(scheme, _)| scheme);
    ["http", "https", "mailto"]
        .iter()
        .any(|known| scheme.eq_ignore_ascii_case(known))
}

/// Passes a request ntdll.dll prepared to the server.
fn server_call(env: &mut Env<'_>, request: u32) -> Outcome {
    crate::server::set_time(env);
    match forward(env, request) {
        Ok(status) => Outcome::Return(status),
        Err(error) => Outcome::Fail(error),
    }
}

fn forward(env: &mut Env<'_>, request: u32) -> Result<u32, String> {
    let fault = |error: adv_cpu::backend::MemFault| error.to_string();
    let mut fixed = [0u8; FIXED_SIZE];
    env.cpu.read(request, &mut fixed).map_err(fault)?;
    let word = |offset| u32::from_le_bytes(fixed[offset..offset + 4].try_into().unwrap());
    let duplicate = (word(0) as i32 == DupHandleRequest::NUMBER)
        .then(|| [word(12), word(16), word(20), word(32)]);
    let close = (word(0) as i32 == CloseHandleRequest::NUMBER).then(|| word(12));
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
        if size != 0 {
            env.cpu.read(pointer, &mut bytes).map_err(fault)?;
        }
        data.push(bytes);
    }
    let reply_pointer = env.cpu.read_u32(request + REPLY_DATA).map_err(fault)?;
    let reply_size = u32::from_le_bytes(fixed[REPLY_SIZE..REPLY_SIZE + 4].try_into().unwrap());
    let mut reply = vec![0; reply_size as usize];

    let parts: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
    let context = env.context;
    let link = env
        .ext
        .try_get_mut::<Link>()
        .ok_or("no server connection")?;
    let size = link
        .call_raw(context, env.cpu, &mut fixed, &parts, &mut reply)
        .map_err(|error| format!("server call failed: {error}"))?;
    if u32::from_le_bytes(fixed[..4].try_into().unwrap()) == 0 {
        if let Some([source, handle, destination, options]) = duplicate {
            let result = u32::from_le_bytes(fixed[8..12].try_into().unwrap());
            crate::section::duplicate_handle(env, source, handle, destination, result, options);
        }
        if let Some(handle) = close {
            crate::section::forget(env, handle);
        }
    }
    crate::server::deliver(env);
    env.cpu.write(request, &fixed).map_err(fault)?;
    // As ntdll.so: a request that takes no reply data leaves the pointer
    // unset, so it is touched only when data came back.
    if size != 0 {
        env.cpu
            .write(reply_pointer, &reply[..size])
            .map_err(fault)?;
    }
    Ok(u32::from_le_bytes(fixed[..4].try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    // spec: HOST-MENU-4
    #[test]
    fn only_web_and_mail_addresses_open() {
        assert!(super::link("https://example.com/"));
        assert!(super::link("HTTP://example.com/"));
        assert!(super::link("mailto:someone@example.com"));
        assert!(!super::link("file:///etc/passwd"));
        assert!(!super::link("/usr/bin/open"));
        assert!(!super::link("xdg-open"));
    }

    // spec: REPORT-DIAG-1
    #[test]
    fn wine_log_tag_parser_keeps_only_channel_and_function() {
        assert_eq!(
            super::parse_wine_log_tag(
                "0024:err:module:import_dll Library C:\\Games\\secret.dll not found"
            ),
            Some(("err", "module", "import_dll"))
        );
        assert_eq!(
            super::parse_wine_log_tag("fixme:d3d:wined3d_check_device_format_conversion stub!"),
            Some(("fixme", "d3d", "wined3d_check_device_format_conversion"))
        );
        assert_eq!(
            super::parse_wine_log_tag("0024:trace:file:CreateFileW C:\\Games\\game.exe"),
            None
        );
    }
}
