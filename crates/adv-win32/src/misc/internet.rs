//! WinINet session configuration and ordinary failures of the offline profile.
use crate::network::{self, Library};
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, win32};
use adv_machine::Env;
mod requests;

pub(super) fn register(reg: &mut Registry) {
    requests::register(reg);
    for api in [
        open_a_ABI,
        open_w_ABI,
        url_a_ABI,
        url_w_ABI,
        close_ABI,
        query_a_ABI,
        query_w_ABI,
        connected_ABI,
    ] {
        reg.register(api);
    }
}

#[win32(wininet, name="InternetOpenA", level=state)]
fn open_a(
    env: &mut Env<'_>,
    agent: AnsiStr,
    access: u32,
    proxy: AnsiStr,
    bypass: AnsiStr,
    flags: u32,
) -> GuestPtr<()> {
    GuestPtr::new(network::open(
        env,
        Library::WinInet,
        agent.0.addr(),
        access,
        proxy.0.addr(),
        bypass.0.addr(),
        flags,
        false,
    ))
}
#[win32(wininet, name="InternetOpenW", level=state)]
fn open_w(
    env: &mut Env<'_>,
    agent: WideStr,
    access: u32,
    proxy: WideStr,
    bypass: WideStr,
    flags: u32,
) -> GuestPtr<()> {
    GuestPtr::new(network::open(
        env,
        Library::WinInet,
        agent.0.addr(),
        access,
        proxy.0.addr(),
        bypass.0.addr(),
        flags,
        true,
    ))
}

fn offline_url(env: &mut Env<'_>, session: u32, url: u32, wide: bool) -> GuestPtr<()> {
    if !network::valid(env, Library::WinInet, session) {
        network::fail(env, 6);
        return GuestPtr::default();
    }
    let units = match network::text(env, url, wide) {
        Ok(value) => value,
        Err(error) => {
            network::fail(env, error);
            return GuestPtr::default();
        }
    };
    let text = String::from_utf16_lossy(&units);
    let Some((scheme, authority)) = text.split_once("://") else {
        network::fail(env, 12005); // ERROR_INTERNET_INVALID_URL
        return GuestPtr::default();
    };
    if !["http", "https", "ftp"]
        .iter()
        .any(|known| scheme.eq_ignore_ascii_case(known))
    {
        network::fail(env, 12006); // ERROR_INTERNET_UNRECOGNIZED_SCHEME
    } else if authority.is_empty() || authority.starts_with('/') {
        network::fail(env, 12005);
    } else {
        // No transport or resolver is present. No request or authentication
        // state is fabricated, and ordinary disconnection is not a capability gap.
        let error = network::url_error(env, session);
        network::fail(env, error);
    }
    GuestPtr::default()
}

#[win32(wininet, name="InternetOpenUrlA", level=state)]
fn url_a(
    env: &mut Env<'_>,
    session: GuestPtr<()>,
    url: AnsiStr,
    _headers: AnsiStr,
    _length: u32,
    _flags: u32,
    _context: u32,
) -> GuestPtr<()> {
    offline_url(env, session.addr(), url.0.addr(), false)
}
#[win32(wininet, name="InternetOpenUrlW", level=state)]
fn url_w(
    env: &mut Env<'_>,
    session: GuestPtr<()>,
    url: WideStr,
    _headers: WideStr,
    _length: u32,
    _flags: u32,
    _context: u32,
) -> GuestPtr<()> {
    offline_url(env, session.addr(), url.0.addr(), true)
}
#[win32(wininet, name="InternetCloseHandle", level=state)]
fn close(env: &mut Env<'_>, handle: GuestPtr<()>) -> i32 {
    network::close(env, Library::WinInet, handle.addr())
}

#[win32(wininet, name="InternetQueryOptionA", level=state)]
fn query_a(
    env: &mut Env<'_>,
    handle: GuestPtr<()>,
    option: u32,
    output: GuestPtr<()>,
    length: GuestPtr<u32>,
) -> i32 {
    network::query(
        env,
        Library::WinInet,
        handle.addr(),
        option,
        output.addr(),
        length.addr(),
        false,
    )
}
#[win32(wininet, name="InternetQueryOptionW", level=state)]
fn query_w(
    env: &mut Env<'_>,
    handle: GuestPtr<()>,
    option: u32,
    output: GuestPtr<()>,
    length: GuestPtr<u32>,
) -> i32 {
    network::query(
        env,
        Library::WinInet,
        handle.addr(),
        option,
        output.addr(),
        length.addr(),
        true,
    )
}

#[win32(wininet, name="InternetGetConnectedState", level=implement)]
fn connected(env: &mut Env<'_>, flags: GuestPtr<u32>, reserved: u32) -> i32 {
    if reserved != 0 {
        return network::fail(env, 87) as i32;
    }
    if !flags.is_null() && flags.write(env.cpu, 0).is_err() {
        return network::fail(env, 998) as i32;
    }
    0
}

#[cfg(test)]
mod tests;
