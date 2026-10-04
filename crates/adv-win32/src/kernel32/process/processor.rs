//! Queries and validated affinity over the one OS-owned virtual CPU profile.
use crate::util::{err, set_last_error};
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::MemFault;
use adv_machine::{Env, Object};
use adv_os::processor::Processors;

fn process(env: &Env<'_>, handle: Handle) -> bool {
    handle.0 == u32::MAX
        || env
            .sys
            .objects
            .host::<super::super::handle::ProcessHandle>(handle.0)
            .is_some()
}
fn thread(env: &Env<'_>, handle: Handle) -> bool {
    handle.0 == u32::MAX - 1 || matches!(env.sys.objects.get(handle.0), Some(Object::Thread { .. }))
}
#[win32(kernel32, name = "GetProcessAffinityMask", level = implement)]
fn get_affinity(
    env: &mut Env<'_>,
    handle: Handle,
    process_out: GuestPtr<u32>,
    system_out: GuestPtr<u32>,
) -> Result<i32, MemFault> {
    if !process(env, handle) {
        set_last_error(env, err::INVALID_HANDLE);
        return Ok(0);
    }
    let p = *env.ext.get_or_default::<Processors>();
    adv_abi::guest::check_range(
        env.cpu,
        process_out.addr(),
        4,
        adv_cpu::backend::Prot::WRITE,
    )?;
    adv_abi::guest::check_range(env.cpu, system_out.addr(), 4, adv_cpu::backend::Prot::WRITE)?;
    process_out.write(env.cpu, p.process_mask())?;
    system_out.write(env.cpu, p.active_mask())?;
    Ok(1)
}
#[win32(kernel32, name = "SetProcessAffinityMask", level = implement)]
fn set_process(env: &mut Env<'_>, handle: Handle, mask: u32) -> i32 {
    if !process(env, handle) {
        set_last_error(env, err::INVALID_HANDLE);
        return 0;
    }
    if !env.ext.get_or_default::<Processors>().accepts_mask(mask) {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    1 // The validated one-CPU mask equals the existing process affinity.
}
#[win32(kernel32, name = "SetThreadAffinityMask", level = implement)]
fn set_thread(env: &mut Env<'_>, handle: Handle, mask: u32) -> u32 {
    if !thread(env, handle) {
        set_last_error(env, err::INVALID_HANDLE);
        return 0;
    }
    let p = env.ext.get_or_default::<Processors>();
    if !p.accepts_mask(mask) {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    p.process_mask() // The only valid requested mask and the previous mask are 1.
}
fn info(env: &mut Env<'_>, output: GuestPtr<[u32; 9]>) -> Result<(), MemFault> {
    let p = *env.ext.get_or_default::<Processors>();
    // Family/model/revision follow the stable guest CPUID 0x00000690. The
    // profile is x86 itself; there is no host WOW64/native-x64 exposure.
    output.write(
        env.cpu,
        [
            0,
            4096,
            0x10000,
            0x7ffeffff,
            p.active_mask(),
            p.count(),
            586,
            65536,
            6 | (0x0900 << 16),
        ],
    )
}
#[win32(kernel32, name = "GetSystemInfo", level = implement)]
fn system_info(env: &mut Env<'_>, output: GuestPtr<[u32; 9]>) -> Result<(), MemFault> {
    info(env, output)
}
#[win32(kernel32, name = "GetNativeSystemInfo", level = implement)]
fn native_info(env: &mut Env<'_>, output: GuestPtr<[u32; 9]>) -> Result<(), MemFault> {
    info(env, output)
}
#[win32(kernel32, name = "GetCurrentProcessorNumber", level = implement)]
fn current_processor(env: &mut Env<'_>) -> u32 {
    env.ext.get_or_default::<Processors>().current()
}
pub(super) fn register(reg: &mut Registry) {
    for api in [
        get_affinity_ABI,
        set_process_ABI,
        set_thread_ABI,
        system_info_ABI,
        native_info_ABI,
        current_processor_ABI,
    ] {
        reg.register(api);
    }
}
