//! Caret creation currently reports the missing visual facility explicitly.
use super::user;
use crate::util::set_last_error;
use adv_abi::{Env, GuestPtr, Handle, Registry, types::POINT, win32};

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_ABI);
    registry.register(destroy_ABI);
    registry.register(show_ABI);
    registry.register(hide_ABI);
    registry.register(set_position_ABI);
    registry.register(get_position_ABI);
}

// CreateCaret cannot publish a caret yet, so these are ordinary missing-caret
// results. Cleanup must not become a diagnostic stub or manufacture an owner.
#[win32(user32, name = "DestroyCaret", level = state)]
fn destroy(env: &mut Env<'_>) -> i32 {
    absent(env, None)
}
#[win32(user32, name = "ShowCaret", level = state)]
fn show(env: &mut Env<'_>, window: Handle) -> i32 {
    absent(env, Some(window.0))
}
#[win32(user32, name = "HideCaret", level = state)]
fn hide(env: &mut Env<'_>, window: Handle) -> i32 {
    absent(env, Some(window.0))
}
#[win32(user32, name = "SetCaretPos", level = state)]
fn set_position(env: &mut Env<'_>, _x: i32, _y: i32) -> i32 {
    absent(env, None)
}

fn absent(env: &mut Env<'_>, window: Option<u32>) -> i32 {
    let invalid = window
        .is_some_and(|hwnd| hwnd != 0 && user(env).windows.get(&env.sys.objects, &hwnd).is_none());
    set_last_error(env, if invalid { 1400 } else { 5 });
    0
}
#[win32(user32, name = "GetCaretPos", level = state)]
fn get_position(env: &mut Env<'_>, point: GuestPtr<POINT>) -> i32 {
    // Native queues retain an initial (0,0) position even without a caret.
    if adv_abi::guest::check_range(env.cpu, point.addr(), 8, adv_cpu::backend::Prot::WRITE).is_err()
        || point.write(env.cpu, POINT { x: 0, y: 0 }).is_err()
    {
        set_last_error(env, 998);
        return 0;
    }
    1
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
