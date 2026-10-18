//! Native thread context uses the same stopped process and architecture owner.
use adv_abi::{GuestPtr, Handle, Registry, win32};
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

fn suspend(env: &mut Env<'_>, thread: Handle, previous: GuestPtr<u32>) -> Outcome {
    crate::kernel32::native_suspend_thread(env, thread.0, previous.addr())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the parameters mirror the guest API"
)]
fn create(
    env: &mut Env<'_>,
    output: GuestPtr<u32>,
    access: u32,
    attributes: GuestPtr<()>,
    process: Handle,
    start: GuestPtr<()>,
    parameter: GuestPtr<()>,
    flags: u32,
    zero_bits: u32,
    stack: u32,
    reserve: u32,
    extended: GuestPtr<()>,
) -> Outcome {
    crate::kernel32::native_create_thread(
        env,
        [
            output.addr(),
            access,
            attributes.addr(),
            process.0,
            start.addr(),
            parameter.addr(),
            flags,
            zero_bits,
            stack,
            reserve,
            extended.addr(),
        ],
    )
}

fn terminate(env: &mut Env<'_>, thread: Handle, status: i32) -> Outcome {
    crate::kernel32::native_terminate_thread(env, thread.0, status as u32)
}

fn exit(env: &mut Env<'_>, status: i32) -> Outcome {
    crate::kernel32::exit_thread(env, status as u32)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the parameters mirror the guest API"
)]
fn rtl_create(
    env: &mut Env<'_>,
    process: Handle,
    security: GuestPtr<()>,
    suspended: u32,
    zero_bits: u32,
    reserve: u32,
    commit: u32,
    start: GuestPtr<()>,
    parameter: GuestPtr<()>,
    thread: GuestPtr<u32>,
    client_id: GuestPtr<()>,
) -> Outcome {
    crate::kernel32::native_rtl_create_thread(
        env,
        [
            process.0,
            security.addr(),
            suspended,
            zero_bits,
            reserve,
            commit,
            start.addr(),
            parameter.addr(),
            thread.addr(),
            client_id.addr(),
        ],
    )
}

#[win32(ntdll, name = "NtDelayExecution", level = implement)]
fn delay(env: &mut Env<'_>, alertable: u32, interval: GuestPtr<i64>) -> Outcome {
    crate::kernel32::apc::delay_native(env, alertable as u8 != 0, interval.addr())
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_context_ABI);
    registry.register(set_context_ABI);
    registry.register(resume_ABI);
    crate::stdcall(registry, "ntdll.dll", "NtSuspendThread", 2, |env| {
        let args = [env.arg(0), env.arg(1)];
        suspend(env, Handle(args[0]), GuestPtr::new(args[1]))
    });
    crate::stdcall(registry, "ntdll.dll", "NtCreateThreadEx", 11, |env| {
        let a: [u32; 11] = std::array::from_fn(|i| env.arg(i as u32));
        create(
            env,
            GuestPtr::new(a[0]),
            a[1],
            GuestPtr::new(a[2]),
            Handle(a[3]),
            GuestPtr::new(a[4]),
            GuestPtr::new(a[5]),
            a[6],
            a[7],
            a[8],
            a[9],
            GuestPtr::new(a[10]),
        )
    });
    crate::stdcall(registry, "ntdll.dll", "NtTerminateThread", 2, |env| {
        let args = [env.arg(0), env.arg(1)];
        terminate(env, Handle(args[0]), args[1] as i32)
    });
    crate::stdcall(registry, "ntdll.dll", "RtlExitUserThread", 1, |env| {
        let code = env.arg(0);
        exit(env, code as i32)
    });
    crate::stdcall(registry, "ntdll.dll", "RtlCreateUserThread", 10, |env| {
        let a: [u32; 10] = std::array::from_fn(|i| env.arg(i as u32));
        rtl_create(
            env,
            Handle(a[0]),
            GuestPtr::new(a[1]),
            a[2],
            a[3],
            a[4],
            a[5],
            GuestPtr::new(a[6]),
            GuestPtr::new(a[7]),
            GuestPtr::new(a[8]),
            GuestPtr::new(a[9]),
        )
    });
    registry.register(delay_ABI);
    crate::stdcall(registry, "ntdll.dll", "NtQueueApcThread", 5, |env| {
        let a: [u32; 5] = std::array::from_fn(|i| env.arg(i as u32));
        crate::kernel32::apc::queue_native(env, a[0], a[1], [a[2], a[3], a[4]])
    });
    crate::stdcall(
        registry,
        "ntdll.dll",
        "NtTestAlert",
        0,
        crate::kernel32::apc::test_alert,
    );
    crate::stdcall(
        registry,
        "ntdll.dll",
        "RtlDispatchAPC",
        3,
        crate::kernel32::apc::dispatch_user,
    );
}
