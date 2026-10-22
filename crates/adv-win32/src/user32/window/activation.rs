//! Activation follows guest NC vetoes and keeps NULL as a real deactivation path.
use super::*;
pub(super) fn request(
    env: &mut Env<'_>,
    hwnd: u32,
    foreground: bool,
    then: impl FnOnce(&mut Env<'_>, bool) -> Outcome + 'static,
) -> Outcome {
    let thread = if hwnd == 0 {
        env.context
    } else {
        let Some(window) = user(env).windows.get(&env.sys.objects, &hwnd) else {
            return then(env, false);
        };
        window.thread
    };
    let old = user_mut(env).queue(thread).active;
    if old == hwnd {
        let u = user_mut(env);
        let restore_foreground = foreground && !u.foreground_active && hwnd != 0;
        if foreground && hwnd != 0 {
            u.foreground = hwnd;
            u.foreground_active = true;
        }
        if restore_foreground {
            return notify(
                env,
                thread,
                hwnd,
                VecDeque::from([
                    [hwnd, wm::ACTIVATEAPP, 1, 0],
                    [hwnd, wm::NCACTIVATE, 1, 0],
                    [hwnd, wm::ACTIVATE, 1, 0],
                ]),
                then,
            );
        }
        return then(env, true);
    }
    let proceed = move |env: &mut Env<'_>, accepted: u32| {
        // A nested activation from the NC callback supersedes the outer request.
        if accepted == 0 || user_mut(env).queue(thread).active != old {
            return then(env, false);
        }
        if hwnd != 0
            && !user(env)
                .windows
                .get(&env.sys.objects, &hwnd)
                .is_some_and(|w| !w.destroying)
        {
            return then(env, false);
        }
        let u = &mut env.ext.get_mut::<Win32>().user;
        let was_foreground = u.foreground_active;
        let foreground_thread = u
            .windows
            .get(&env.sys.objects, &u.foreground)
            .map(|w| w.thread);
        let update_foreground = foreground || was_foreground && foreground_thread == Some(thread);
        if hwnd != 0 {
            z_order::raise(u, &mut env.sys.objects, hwnd)
        }
        if update_foreground {
            u.foreground = hwnd;
            u.foreground_active = hwnd != 0;
        }
        u.queue(thread).active = hwnd;
        let minimized = |handle: u32| {
            u.windows
                .get(&env.sys.objects, &handle)
                .is_some_and(|w| w.style & WS_MINIMIZE != 0)
        };
        let mut msgs = VecDeque::new();
        if old != 0 {
            msgs.push_back([old, wm::ACTIVATE, u32::from(minimized(old)) << 16, hwnd]);
        }
        if hwnd != 0 {
            if update_foreground && !was_foreground {
                msgs.push_back([hwnd, wm::ACTIVATEAPP, 1, 0]);
            }
            msgs.push_back([hwnd, wm::NCACTIVATE, 1, 0]);
            msgs.push_back([
                hwnd,
                wm::ACTIVATE,
                1 | (u32::from(minimized(hwnd)) << 16),
                old,
            ]);
        }
        notify(env, thread, hwnd, msgs, move |env, accepted| {
            if accepted && hwnd == 0 && thread == env.context {
                set_focus(env, 0, move |env, _| then(env, true))
            } else {
                then(env, accepted)
            }
        })
    };
    if old != 0 {
        send(env, old, wm::NCACTIVATE, 0, 0, proceed)
    } else {
        proceed(env, 1)
    }
}

fn notify(
    env: &mut Env<'_>,
    thread: u32,
    expected: u32,
    mut messages: VecDeque<[u32; 4]>,
    then: impl FnOnce(&mut Env<'_>, bool) -> Outcome + 'static,
) -> Outcome {
    if user_mut(env).queue(thread).active != expected {
        return then(env, false);
    }
    if let Some([window, message, wparam, lparam]) = messages.pop_front() {
        send(env, window, message, wparam, lparam, move |env, _| {
            notify(env, thread, expected, messages, then)
        })
    } else {
        then(env, true)
    }
}
