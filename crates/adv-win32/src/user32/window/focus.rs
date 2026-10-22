//! Focus uses the thread queue and activates the target's existing root window.
use super::*;
type Then = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;
fn eligible(env: &Env<'_>, thread: u32, mut hwnd: u32) -> Result<(), u32> {
    while hwnd != 0 {
        let window = user(env)
            .windows
            .get(&env.sys.objects, &hwnd)
            .ok_or(1400u32)?;
        if window.destroying {
            return Err(1400);
        }
        if window.thread != thread || !window.enabled() {
            return Err(87);
        }
        hwnd = window.parent;
    }
    Ok(())
}
pub(super) fn request(
    env: &mut Env<'_>,
    hwnd: u32,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    request_boxed(env, hwnd, Box::new(then))
}
fn request_boxed(env: &mut Env<'_>, hwnd: u32, then: Then) -> Outcome {
    let thread = env.context;
    if let Err(error) = eligible(env, thread, hwnd) {
        set_last_error(env, error);
        return then(env, 0);
    }
    let q = user_mut(env).queue(thread);
    let old = q.focus;
    q.focus_request = q.focus_request.wrapping_add(1);
    let request = q.focus_request;
    q.activation_focus = None;
    let root = user(env).root(&env.sys.objects, hwnd);
    if hwnd != 0 && user_mut(env).queue(thread).active != root {
        user_mut(env).queue(thread).activation_focus = Some(hwnd);
        return activation::request(env, root, false, move |env, accepted| {
            let q = user_mut(env).queue(thread);
            if q.focus_request != request {
                return then(env, 0);
            }
            q.activation_focus = None;
            if !accepted {
                return then(env, 0);
            }
            apply(env, thread, hwnd, old, request, then)
        });
    }
    apply(env, thread, hwnd, old, request, then)
}
fn apply(
    env: &mut Env<'_>,
    thread: u32,
    hwnd: u32,
    original: u32,
    request: u64,
    then: Then,
) -> Outcome {
    if let Err(error) = eligible(env, thread, hwnd) {
        set_last_error(env, error);
        return then(env, 0);
    }
    let q = user_mut(env).queue(thread);
    let old = q.focus;
    if old == hwnd {
        return then(env, original);
    }
    q.focus = hwnd;
    let notify = move |env: &mut Env<'_>, _: u32| {
        let q = user_mut(env).queue(thread);
        if hwnd != 0 && q.focus == hwnd && q.focus_request == request {
            send(env, hwnd, wm::SETFOCUS, old, 0, move |env, _| {
                then(env, original)
            })
        } else {
            then(env, original)
        }
    };
    if old != 0 {
        send(env, old, wm::KILLFOCUS, hwnd, 0, notify)
    } else {
        notify(env, 0)
    }
}
/// DefWindowProc uses the requested child directly during SetFocus activation.
pub(in crate::user32) fn activate(
    env: &mut Env<'_>,
    hwnd: u32,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let thread = env.context;
    let pending = user_mut(env).queue(thread).activation_focus;
    if let Some(target) = pending
        && user(env).root(&env.sys.objects, target) == hwnd
    {
        let q = user_mut(env).queue(thread);
        q.activation_focus = None;
        let (old, request) = (q.focus, q.focus_request);
        apply(
            env,
            thread,
            target,
            old,
            request,
            Box::new(move |env, _| then(env)),
        )
    } else {
        request(env, hwnd, move |env, _| then(env))
    }
}
pub(super) fn cancel_subtree(
    env: &mut Env<'_>,
    hwnd: u32,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let Some(thread) = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .map(|w| w.thread)
    else {
        return then(env);
    };
    let old = user_mut(env).queue(thread).focus;
    let mut current = old;
    while current != 0 {
        if current == hwnd {
            let q = user_mut(env).queue(thread);
            q.focus_request = q.focus_request.wrapping_add(1);
            q.activation_focus = None;
            let request = q.focus_request;
            return apply(
                env,
                thread,
                0,
                old,
                request,
                Box::new(move |env, _| then(env)),
            );
        }
        current = user(env)
            .windows
            .get(&env.sys.objects, &current)
            .map_or(0, |w| w.parent);
    }
    then(env)
}
