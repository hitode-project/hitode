//! Library-owned Internet sessions in the shared object table. The offline
//! profile has no transport; allocating configuration is independent of DNS or I/O.
use adv_abi::{AnsiStr, GuestPtr, WideStr, guest::check_range};
use adv_cpu::backend::Prot;
use adv_machine::{Env, objects::Namespace};

use crate::{Win32, util::set_last_error};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Library {
    WinInet,
    WinHttp,
}

struct Sessions(Namespace);
struct Session {
    library: Library,
    agent: Vec<u16>,
    cache_only: bool,
    timeouts: [u32; 3],
}

pub(crate) fn text(env: &Env<'_>, pointer: u32, wide: bool) -> Result<Vec<u16>, u32> {
    if pointer == 0 {
        return Ok(Vec::new());
    }
    if wide {
        WideStr(GuestPtr::new(pointer))
            .read(env.cpu, 32768)
            .map_err(|_| 998)
    } else {
        AnsiStr(GuestPtr::new(pointer))
            .read(env.cpu, 32768)
            .map(|bytes| env.ext.get::<Win32>().codec.to_utf16(0, &bytes))
            .map_err(|_| 998)
    }
}

pub(crate) fn fail(env: &mut Env<'_>, error: u32) -> u32 {
    set_last_error(env, error);
    0
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn open(
    env: &mut Env<'_>,
    library: Library,
    agent: u32,
    access: u32,
    proxy: u32,
    bypass: u32,
    flags: u32,
    wide: bool,
) -> u32 {
    let api = match (library, wide) {
        (Library::WinInet, false) => "wininet.dll!InternetOpenA",
        (Library::WinInet, true) => "wininet.dll!InternetOpenW",
        (Library::WinHttp, _) => "winhttp.dll!WinHttpOpen",
    };
    if !matches!(access, 0 | 1 | 3) && !(library == Library::WinInet && access == 4) {
        return fail(env, 87);
    }
    let allowed = if library == Library::WinInet {
        0x11000000
    } else {
        0x10000000
    };
    if flags & !allowed != 0 {
        return fail(env, 87);
    }
    if flags & 0x10000000 != 0 {
        adv_abi::warn_unavailable(env, api, "asynchronous Internet callbacks are unavailable");
        return fail(env, 50);
    }
    let agent = match text(env, agent, wide) {
        Ok(value) => value,
        Err(error) => return fail(env, error),
    };
    // A named proxy is input configuration, not a connection or authentication.
    // Validate it even though the offline profile cannot reach it.
    if access == 3 {
        let proxy = match text(env, proxy, wide) {
            Ok(value) => value,
            Err(error) => return fail(env, error),
        };
        if proxy.is_empty() {
            return fail(env, 87);
        }
        if let Err(error) = text(env, bypass, wide) {
            return fail(env, error);
        }
    }
    let namespace = if let Some(owner) = env.ext.try_get::<Sessions>() {
        owner.0
    } else {
        let Some(namespace) = env.sys.objects.new_namespace_in(0x0d00_0004, 0x0dff_fffc) else {
            return fail(env, 8);
        };
        env.ext.insert(Sessions(namespace));
        namespace
    };
    match env.sys.objects.insert_host_in(
        namespace,
        Session {
            library,
            agent,
            cache_only: flags & 0x01000000 != 0,
            timeouts: [60_000, 30_000, 30_000],
        },
    ) {
        Some(handle) => handle,
        None => fail(env, 8),
    }
}

pub(crate) fn valid(env: &Env<'_>, library: Library, handle: u32) -> bool {
    session(env, library, handle).is_some()
}
fn session<'a>(env: &'a Env<'_>, library: Library, handle: u32) -> Option<&'a Session> {
    let namespace = env.ext.try_get::<Sessions>()?.0;
    let identity = env.sys.objects.lookup_alias::<Session>(namespace, handle)?;
    env.sys
        .objects
        .owned(identity)
        .filter(|value| value.library == library)
}

/// The profile starts with an empty WinINet cache. Cache-only sessions never
/// attempt a network request, and a missing cache item has the file-not-found error.
pub(crate) fn url_error(env: &Env<'_>, handle: u32) -> u32 {
    if session(env, Library::WinInet, handle).is_some_and(|value| value.cache_only) {
        2
    } else {
        12029
    }
}

pub(crate) fn close(env: &mut Env<'_>, library: Library, handle: u32) -> i32 {
    if !valid(env, library, handle) {
        return fail(env, 6) as i32;
    }
    let namespace = env.ext.get::<Sessions>().0;
    env.sys.objects.close_alias(namespace, handle);
    1
}

pub(crate) fn query(
    env: &mut Env<'_>,
    library: Library,
    handle: u32,
    option: u32,
    buffer: u32,
    length: u32,
    wide: bool,
) -> i32 {
    let Some(session) = session(env, library, handle) else {
        return fail(env, 6) as i32;
    };
    let bytes = match option {
        2 | 5 | 6 if library == Library::WinInet => session.timeouts[match option {
            2 => 0,
            5 => 1,
            _ => 2,
        }]
        .to_le_bytes()
        .to_vec(),
        9 => 1u32.to_le_bytes().to_vec(), // INTERNET_HANDLE_TYPE_INTERNET / WINHTTP_HANDLE_TYPE_SESSION
        41 => {
            let mut units = session.agent.clone();
            units.push(0);
            if wide {
                units.into_iter().flat_map(u16::to_le_bytes).collect()
            } else {
                env.ext.get::<Win32>().codec.from_utf16(0, &units).0
            }
        }
        _ => {
            adv_abi::warn_unavailable(
                env,
                match (library, wide) {
                    (Library::WinInet, false) => "wininet.dll!InternetQueryOptionA",
                    (Library::WinInet, true) => "wininet.dll!InternetQueryOptionW",
                    (Library::WinHttp, _) => "winhttp.dll!WinHttpQueryOption",
                },
                &format!("Internet session option {option} is unavailable"),
            );
            return fail(env, 12009) as i32;
        }
    };
    let size = GuestPtr::<u32>::new(length);
    let Ok(capacity) = size.read(env.cpu) else {
        return fail(env, 998) as i32;
    };
    if check_range(env.cpu, length, 4, Prot::WRITE).is_err() {
        return fail(env, 998) as i32;
    }
    if buffer == 0 || capacity < bytes.len() as u32 {
        let _ = size.write(env.cpu, bytes.len() as u32);
        return fail(env, 122) as i32;
    }
    if check_range(env.cpu, buffer, bytes.len() as u32, Prot::WRITE).is_err() {
        return fail(env, 998) as i32;
    }
    if env.cpu.write(buffer, &bytes).is_err() || size.write(env.cpu, bytes.len() as u32).is_err() {
        return fail(env, 998) as i32;
    }
    1
}

pub(crate) fn set_option(
    env: &mut Env<'_>,
    handle: u32,
    option: u32,
    input: u32,
    length: u32,
) -> i32 {
    if !valid(env, Library::WinInet, handle) {
        return fail(env, 6) as i32;
    }
    if !matches!(option, 2 | 5 | 6 | 41) {
        adv_abi::warn_unavailable(
            env,
            "wininet.dll!InternetSetOptionW",
            &format!("Internet session option {option} is unavailable"),
        );
        return fail(env, 12009) as i32;
    }
    if (option != 41 && length != 4) || (option == 41 && length & 1 != 0) {
        return fail(env, 87) as i32;
    }
    if input == 0 || check_range(env.cpu, input, length, Prot::READ).is_err() {
        return fail(env, 87) as i32;
    }
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(length as usize).is_err() {
        return fail(env, 8) as i32;
    }
    bytes.resize(length as usize, 0);
    if env.cpu.read(input, &mut bytes).is_err() {
        return fail(env, 998) as i32;
    }
    let namespace = env.ext.get::<Sessions>().0;
    let identity = env
        .sys
        .objects
        .lookup_alias::<Session>(namespace, handle)
        .unwrap();
    let session = env.sys.objects.owned_mut::<Session>(identity).unwrap();
    if option == 41 {
        session.agent = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| u16::from_le_bytes(*bytes))
            .take_while(|unit| *unit != 0)
            .collect();
    } else {
        let index = match option {
            2 => 0,
            5 => 1,
            _ => 2,
        };
        session.timeouts[index] = u32::from_le_bytes(bytes.try_into().unwrap());
    }
    1
}
