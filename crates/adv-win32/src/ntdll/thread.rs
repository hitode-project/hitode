//! Native thread context uses the same stopped process and architecture owner.
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};

#[win32(ntdll, name = "NtGetContextThread", level = implement)]
fn get_context(env: &mut Env<'_>, thread: Handle, context: GuestPtr<()>) -> Outcome {
    crate::kernel32::native_thread_context(env, thread.0, context.addr(), false)
}

#[win32(ntdll, name = "NtSetContextThread", level = implement)]
fn set_context(env: &mut Env<'_>, thread: Handle, context: GuestPtr<()>) -> Outcome {
    crate::kernel32::native_thread_context(env, thread.0, context.addr(), true)
}

#[win32(ntdll, name = "NtResumeThread", level = implement)]
fn resume(env: &mut Env<'_>, thread: Handle, previous: GuestPtr<u32>) -> Outcome {
    crate::kernel32::native_resume_thread(env, thread.0, previous.addr())
}

#[win32(ntdll, name = "NtDelayExecution", level = implement)]
fn delay(env: &mut Env<'_>, alertable: u32, interval: GuestPtr<i64>) -> Outcome {
    if interval.addr() & 3 != 0 {
        return Outcome::Return(0x80000002);
    }
    if check_range(env.cpu, interval.addr(), 8, Prot::READ).is_err() {
        return Outcome::Return(0xc0000005);
    }
    let Ok(interval) = interval.read(env.cpu) else {
        return Outcome::Return(0xc0000005);
    };
    if alertable as u8 != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!NtDelayExecution",
            "alertable APC delivery is unavailable",
        );
        return Outcome::Return(0xc00000bb);
    }
    if interval == 0 {
        let other_ready = env
            .sys
            .contexts
            .iter()
            .any(|c| c.id != env.context && c.is_ready());
        return Outcome::block_then_return(
            adv_machine::Wait::Yield,
            if other_ready { 0 } else { 0x40000024 },
        );
    }
    let remaining = if interval < 0 {
        interval.unsigned_abs()
    } else {
        (interval as u64).saturating_sub(crate::kernel32::filetime_now(env))
    };
    // Native intervals use 100 ns units. Round up to the shared microsecond clock.
    let until = env
        .sys
        .clock
        .now_us()
        .saturating_add(remaining.div_ceil(10));
    crate::kernel32::delay_until(env, until)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_context_ABI);
    registry.register(set_context_ABI);
    registry.register(resume_ABI);
    registry.register(delay_ABI);
}
