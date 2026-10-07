//! Caret creation currently reports the missing visual facility explicitly.
use super::user;
use crate::util::set_last_error;
use adv_abi::{Env, Handle, Registry, win32};

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_ABI);
}

#[win32(user32, name = "CreateCaret", level = safe_fail)]
fn create(env: &mut Env<'_>, window: Handle, _bitmap: Handle, _width: i32, _height: i32) -> i32 {
    if user(env).windows.get(&env.sys.objects, &window.0).is_none() {
        set_last_error(env, 1400);
        return 0;
    }
    // Never claim that a requested insertion marker has been painted. The
    // caller may continue without a caret when this visual facility is absent.
    adv_abi::warn_unavailable(
        env,
        "user32.dll!CreateCaret",
        "caret painting is unavailable",
    );
    set_last_error(env, 50);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, GuestExt, Prot, Reg},
    };
    use adv_machine::{Clock, Machine, Outcome, layout::PARK_ADDR};

    #[test]
    fn create_caret_safe_failure_is_available_and_leaves_user_ownership_untouched() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        assert!(registry.dynamic_available("user32.dll", "CreateCaret"));
        let api = registry.get("user32.dll", "CreateCaret").unwrap();
        assert_eq!(api.conv, adv_abi::Conv::Stdcall { argc: 4 });
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/scratch/caret-tests");
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(&root, "authored.exe", &root),
        )
        .unwrap();
        machine.spawn("caret", PARK_ADDR, &[], 65536).unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        let data = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, 4096, Prot::RW)
            .unwrap();
        machine.cpu.set_reg(Reg::Esp, data);
        let hwnd = machine.ext.get::<crate::Win32>().user.desktop;
        let objects = machine.sys.objects.object_count();
        let queues = machine.ext.get::<crate::Win32>().user.queues.len();
        for (i, value) in [hwnd, 0, 1, 1].into_iter().enumerate() {
            machine
                .cpu
                .write_u32(data + 4 + i as u32 * 4, value)
                .unwrap();
        }
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        assert!(matches!((api.handler)(&mut env), Outcome::Return(0)));
        assert_eq!(crate::util::last_error(&env), 50);
        assert_eq!(env.sys.objects.object_count(), objects);
        assert_eq!(user(&env).queues.len(), queues);
        assert_eq!(env.ext.get::<adv_abi::ApiDiagnostics>().warnings.len(), 1);
        env.cpu.write_u32(data + 4, 0).unwrap();
        assert!(matches!((api.handler)(&mut env), Outcome::Return(0)));
        assert_eq!(crate::util::last_error(&env), 1400);
        assert_eq!(env.ext.get::<adv_abi::ApiDiagnostics>().warnings.len(), 1);
    }
}
