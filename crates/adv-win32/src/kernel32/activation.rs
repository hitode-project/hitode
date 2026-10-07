//! Activation contexts are unavailable until the common loader owns SxS binding.
//! Fail creation explicitly so callers can use their ordinary loader fallback.
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;

fn create(env: &mut Env<'_>, context: GuestPtr<()>, api: &'static str) -> Handle {
    let error = if check_range(env.cpu, context.addr(), 32, Prot::READ).is_err() {
        998
    } else if env.cpu.read_u32(context.addr()).ok() != Some(32)
        || env.cpu.read_u32(context.addr() + 4).unwrap_or(u32::MAX) & !0xff != 0
    {
        87
    } else {
        warn_unavailable(
            env,
            api,
            "side-by-side activation context binding is unavailable",
        );
        14001 // ERROR_SXS_CANT_GEN_ACTCTX
    };
    crate::util::set_last_error(env, error);
    Handle(u32::MAX)
}

// https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-createactctxw
#[win32(kernel32, name = "CreateActCtxW", level = safe_fail)]
fn create_w(env: &mut Env<'_>, context: GuestPtr<()>) -> Handle {
    create(env, context, "kernel32.dll!CreateActCtxW")
}
#[win32(kernel32, name = "CreateActCtxA", level = safe_fail)]
fn create_a(env: &mut Env<'_>, context: GuestPtr<()>) -> Handle {
    create(env, context, "kernel32.dll!CreateActCtxA")
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(create_w_ABI);
    registry.register(create_a_ABI);
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{JitCpu, backend::Guest};
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};

    #[test]
    fn creation_failure_never_fabricates_a_context() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        assert!(registry.dynamic_available("kernel32.dll", "CreateActCtxW"));
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.spawn("activation", PARK_ADDR, &[], 65536).unwrap();
        let data = 0x410000;
        machine.cpu.map(data, 4096, Prot::RW).unwrap();
        for (pointer, size, flags, expected) in [
            (0, 32, 0, 998),
            (data, 31, 0, 87),
            (data, 32, 0x100, 87),
            (data, 32, 0, 14001),
        ] {
            machine.cpu.write_u32(data, size).unwrap();
            machine.cpu.write_u32(data + 4, flags).unwrap();
            let mut env = Env {
                context: 0,
                cpu: &mut machine.cpu,
                sys: &mut machine.sys,
                ext: &mut machine.ext,
                thunk: 0,
            };
            assert_eq!(create_w(&mut env, GuestPtr::new(pointer)).0, u32::MAX);
            assert_eq!(crate::util::last_error(&env), expected);
        }
    }
}
