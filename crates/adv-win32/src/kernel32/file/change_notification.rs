//! Explicit failure fronts until directory notifications share a VFS owner.
use super::*;
use adv_abi::{AnsiStr, Handle, WideStr, win32};

fn unavailable(env: &mut Env<'_>, api: &str) -> Outcome {
    // Never publish an inert waitable handle: a successful notification must
    // observe actual changes in the guest's merged directory view.
    adv_abi::warn_unavailable(
        env,
        api,
        "VFS directory change notifications are unavailable",
    );
    set_last_error(env, 50); // ERROR_NOT_SUPPORTED
    Outcome::Return(INVALID_HANDLE_VALUE)
}

#[win32(kernel32, name = "FindFirstChangeNotificationA", level = safe_fail)]
fn first_a(env: &mut Env<'_>, _path: AnsiStr, _subtree: i32, _filter: u32) -> Outcome {
    unavailable(env, "kernel32.dll!FindFirstChangeNotificationA")
}

#[win32(kernel32, name = "FindFirstChangeNotificationW", level = safe_fail)]
fn first_w(env: &mut Env<'_>, _path: WideStr, _subtree: i32, _filter: u32) -> Outcome {
    unavailable(env, "kernel32.dll!FindFirstChangeNotificationW")
}

fn invalid_handle(env: &mut Env<'_>) -> Outcome {
    // This frontend never creates notification handles. Do not consume or
    // close an unrelated object passed to either lifecycle operation.
    set_last_error(env, err::INVALID_HANDLE);
    Outcome::Return(0)
}

#[win32(kernel32, name = "FindNextChangeNotification", level = safe_fail)]
fn next(env: &mut Env<'_>, _handle: Handle) -> Outcome {
    invalid_handle(env)
}

#[win32(kernel32, name = "FindCloseChangeNotification", level = safe_fail)]
fn close(env: &mut Env<'_>, _handle: Handle) -> Outcome {
    invalid_handle(env)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(first_a_ABI);
    registry.register(first_w_ABI);
    registry.register(next_ABI);
    registry.register(close_ABI);
}

#[cfg(test)]
mod tests {
    use super::super::locking::tests::{args, env, fixture, returned};
    use super::*;

    #[test]
    fn unavailable_notifications_do_not_allocate_or_close_other_handles() {
        let (mut machine, root) = fixture("change-notification-failure");
        std::fs::write(root.join("game/data.bin"), b"preserved").unwrap();
        let file = open(
            &mut env(&mut machine),
            "data.bin",
            0x8000_0000,
            OPEN_EXISTING,
        );
        assert_ne!(file, INVALID_HANDLE_VALUE);
        let count = machine.sys.objects.object_count();
        for handler in [first_a_ABI.handler, first_w_ABI.handler] {
            args(&mut machine, &[0, 1, 0x17f]);
            assert_eq!(
                returned(handler(&mut env(&mut machine))),
                INVALID_HANDLE_VALUE
            );
            assert_eq!(crate::util::last_error(&env(&mut machine)), 50);
            assert_eq!(machine.sys.objects.object_count(), count);
        }
        for handle in [0, INVALID_HANDLE_VALUE, file] {
            for handler in [next_ABI.handler, close_ABI.handler] {
                args(&mut machine, &[handle]);
                assert_eq!(returned(handler(&mut env(&mut machine))), 0);
                assert_eq!(crate::util::last_error(&env(&mut machine)), 6);
                assert_eq!(machine.sys.objects.object_count(), count);
                assert!(machine.sys.objects.host::<FileObj>(file).is_some());
            }
        }
        drop(machine);
        std::fs::remove_dir_all(root).unwrap();
    }
}
