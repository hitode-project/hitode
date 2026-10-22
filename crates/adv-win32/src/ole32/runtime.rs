//! COM and OLE initialization follows each virtual thread, with process server lifetime state.
use super::*;
use adv_abi::guest::check_range;
use adv_cpu::backend::{GuestExt, Prot};
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};
#[derive(Default)]
struct Apartment {
    mode: u32,
    com: u32,
    ole: u32,
}
struct Thread {
    completion: Weak<adv_machine::process::Completion>,
    apartment: Apartment,
}
#[derive(Default)]
struct State {
    threads: BTreeMap<u32, Thread>,
    servers: u32,
    suspended: bool,
}
fn apartment<'a>(env: &'a mut Env<'_>) -> &'a mut Apartment {
    let completion = Arc::downgrade(&env.ctx().completion);
    let context = env.context;
    let state = env.ext.get_or_default::<State>();
    state.threads.retain(|_, thread| {
        thread
            .completion
            .upgrade()
            .is_some_and(|c| c.exit_code().is_none())
    });
    &mut state
        .threads
        .entry(context)
        .or_insert_with(|| Thread {
            completion,
            apartment: Apartment::default(),
        })
        .apartment
}
pub(super) fn initialize(env: &mut Env<'_>, reserved: u32, flags: u32, ole: bool) -> u32 {
    if reserved != 0 || flags & !0xe != 0 {
        return 0x80070057;
    }
    let a = apartment(env);
    let mode = flags & 2;
    if a.com != 0 && a.mode != mode {
        return 0x80010106;
    }
    let Some(count) = a.com.checked_add(1) else {
        return 0x8007000e;
    };
    let result = u32::from(a.com != 0);
    a.mode = mode;
    a.com = count;
    if ole {
        a.ole += 1;
    }
    result
}
pub(super) fn uninitialize(env: &mut Env<'_>, ole: bool) {
    let a = apartment(env);
    if ole {
        if a.ole == 0 {
            return;
        }
        a.ole -= 1;
    }
    a.com = a.com.saturating_sub(1);
    if a.com == 0 {
        a.ole = 0;
    }
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        ole_initialize_ABI,
        ole_uninitialize_ABI,
        add_server_ABI,
        release_server_ABI,
        resume_ABI,
        suspend_ABI,
        error_info_ABI,
    ] {
        registry.register(api);
    }
}
#[win32(ole32,name="OleInitialize",level=implement)]
fn ole_initialize(env: &mut Env<'_>, reserved: GuestPtr<()>) -> u32 {
    initialize(env, reserved.addr(), 2, true)
}
#[win32(ole32,name="OleUninitialize",level=implement)]
fn ole_uninitialize(env: &mut Env<'_>) {
    uninitialize(env, true);
}
#[win32(ole32,name="CoAddRefServerProcess",level=state)]
fn add_server(env: &mut Env<'_>) -> u32 {
    let s = env.ext.get_or_default::<State>();
    s.servers = s.servers.saturating_add(1);
    s.servers
}
#[win32(ole32,name="CoReleaseServerProcess",level=state)]
fn release_server(env: &mut Env<'_>) -> u32 {
    let s = env.ext.get_or_default::<State>();
    s.servers = s.servers.saturating_sub(1);
    if s.servers == 0 {
        s.suspended = true;
    }
    s.servers
}
#[win32(ole32,name="CoResumeClassObjects",level=state)]
fn resume(env: &mut Env<'_>) -> u32 {
    env.ext.get_or_default::<State>().suspended = false;
    0
}
#[win32(ole32,name="CoSuspendClassObjects",level=state)]
fn suspend(env: &mut Env<'_>) -> u32 {
    env.ext.get_or_default::<State>().suspended = true;
    0
}
#[win32(oleaut32,name="GetErrorInfo",level=implement)]
fn error_info(env: &mut Env<'_>, reserved: u32, output: GuestPtr<u32>) -> u32 {
    if reserved != 0 {
        return 0x80070057;
    }
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return 0x80070057;
    }
    // No installed server supplies IErrorInfo. The calling thread has no error object.
    // Do not manufacture an error interface from an HRESULT or from another thread.
    match env.cpu.write_u32(output.addr(), 0) {
        Ok(()) => 1,
        Err(_) => 0x80070057,
    }
}
