//! Offline connections and typed request-handle failures share session ownership.
use crate::network::{self, Library};
use adv_abi::{Registry, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

fn request_error(env: &Env<'_>, handle: u32) -> u32 {
    if network::valid(env, Library::WinInet, handle) {
        12018
    } else {
        6
    }
}

fn request(env: &mut Env<'_>) -> Outcome {
    let error = request_error(env, env.arg(0));
    Outcome::Return(network::fail(env, error))
}

fn write(env: &mut Env<'_>) -> Outcome {
    if !network::valid(env, Library::WinInet, env.arg(0)) {
        return Outcome::Return(network::fail(env, 6));
    }
    let count = env.arg(3);
    if env.arg(1) == 0
        || env.arg(2) == 0
        || count == 0
        || check_range(env.cpu, count, 4, Prot::WRITE).is_err()
    {
        return Outcome::Return(network::fail(env, 87));
    }
    // Native clears writes after argument validation, then rejects the session
    // as an invalid transfer handle. Reads reject its type without touching count.
    let _ = env.cpu.write_u32(count, 0);
    Outcome::Return(network::fail(env, 6))
}

fn connect(env: &mut Env<'_>) -> Outcome {
    if !network::valid(env, Library::WinInet, env.arg(0)) {
        return Outcome::Return(network::fail(env, 6));
    }
    if !matches!(env.arg(5), 1..=3) {
        return Outcome::Return(network::fail(env, 87));
    }
    for index in [1, 3, 4] {
        match network::text(env, env.arg(index), true) {
            Ok(name) if index == 1 && name.is_empty() => {
                return Outcome::Return(network::fail(env, 87));
            }
            Ok(_) => (),
            Err(error) => return Outcome::Return(network::fail(env, error)),
        }
    }
    // The configured machine has no external network, shares or remote hosts.
    // It cannot publish a connection or authenticated request handle.
    Outcome::Return(network::fail(env, 12029))
}

fn error_dialog(env: &mut Env<'_>) -> Outcome {
    let parent = env.arg(0);
    if parent != 0
        && !env
            .ext
            .get::<crate::Win32>()
            .user
            .windows
            .contains_key(&env.sys.objects, &parent)
    {
        return Outcome::Return(6);
    }
    if env.arg(2) == 0 {
        return Outcome::Return(0);
    }
    let handle = env.arg(1);
    if handle != 0 {
        return Outcome::Return(request_error(env, handle));
    }
    adv_abi::warn_unavailable(
        env,
        "wininet.dll!InternetErrorDlg",
        "Internet authentication and certificate error dialogs are unavailable",
    );
    Outcome::Return(50)
}

pub(super) fn register(registry: &mut Registry) {
    for (name, count) in [
        ("HttpEndRequestW", 4),
        ("HttpOpenRequestW", 8),
        ("HttpQueryInfoW", 5),
        ("HttpSendRequestExW", 5),
        ("HttpSendRequestW", 5),
        ("InternetReadFile", 4),
    ] {
        crate::stdcall(registry, "wininet.dll", name, count, request);
    }
    crate::stdcall(
        registry,
        "wininet.dll",
        "InternetSetFilePointer",
        5,
        |env| {
            let error = request_error(env, env.arg(0));
            network::fail(env, error);
            Outcome::Return(u32::MAX)
        },
    );
    crate::stdcall(
        registry,
        "wininet.dll",
        "HttpAddRequestHeadersW",
        4,
        |env| {
            if env.arg(1) == 0 {
                return Outcome::Return(network::fail(env, 87));
            }
            request(env)
        },
    );
    crate::stdcall(
        registry,
        "wininet.dll",
        "InternetQueryDataAvailable",
        4,
        |env| {
            if !network::valid(env, Library::WinInet, env.arg(0)) {
                return Outcome::Return(network::fail(env, 6));
            }
            if check_range(env.cpu, env.arg(1), 4, Prot::WRITE).is_err() {
                return Outcome::Return(network::fail(env, 87));
            }
            let _ = env.cpu.write_u32(env.arg(1), 0);
            request(env)
        },
    );
    crate::stdcall(registry, "wininet.dll", "InternetWriteFile", 4, write);
    crate::stdcall(registry, "wininet.dll", "InternetConnectW", 8, connect);
    crate::stdcall(
        registry,
        "wininet.dll",
        "InternetAttemptConnect",
        1,
        |env| Outcome::Return(if env.arg(0) == 0 { 12029 } else { 87 }),
    );
    crate::stdcall(registry, "wininet.dll", "InternetErrorDlg", 5, error_dialog);
    crate::stdcall(registry, "wininet.dll", "InternetSetOptionW", 4, |env| {
        Outcome::Return(
            network::set_option(env, env.arg(0), env.arg(1), env.arg(2), env.arg(3)) as u32,
        )
    });
}

#[cfg(test)]
mod tests;
