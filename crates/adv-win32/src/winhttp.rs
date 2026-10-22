//! WinHTTP session ownership is independent of an available network transport.
use crate::network::{self, Library};
use adv_abi::{GuestPtr, Registry, WideStr, win32};
use adv_machine::Env;

pub(crate) fn register(registry: &mut Registry) {
    for api in [open_ABI, close_ABI, query_ABI, platform_ABI] {
        registry.register(api);
    }
}

#[win32(winhttp, name="WinHttpOpen", level=state)]
fn open(
    env: &mut Env<'_>,
    agent: WideStr,
    access: u32,
    proxy: WideStr,
    bypass: WideStr,
    flags: u32,
) -> GuestPtr<()> {
    GuestPtr::new(network::open(
        env,
        Library::WinHttp,
        agent.0.addr(),
        access,
        proxy.0.addr(),
        bypass.0.addr(),
        flags,
        true,
    ))
}
#[win32(winhttp, name="WinHttpCloseHandle", level=state)]
fn close(env: &mut Env<'_>, handle: GuestPtr<()>) -> i32 {
    network::close(env, Library::WinHttp, handle.addr())
}
#[win32(winhttp, name="WinHttpQueryOption", level=state)]
fn query(
    env: &mut Env<'_>,
    handle: GuestPtr<()>,
    option: u32,
    output: GuestPtr<()>,
    length: GuestPtr<u32>,
) -> i32 {
    network::query(
        env,
        Library::WinHttp,
        handle.addr(),
        option,
        output.addr(),
        length.addr(),
        true,
    )
}
#[win32(winhttp, name="WinHttpCheckPlatform", level=implement)]
fn platform(_env: &mut Env<'_>) -> i32 {
    1
}
