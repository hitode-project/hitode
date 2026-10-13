//! Rectangular USER hit testing over the existing window and z-order owner.
//! Custom WM_NCHITTEST callbacks, shaped regions and layered alpha hit regions
//! are outside this geometric model; no callback result is synthesized here.
use super::{User, user, window::Window};
use adv_abi::{Env, Handle, Registry, types::POINT, win32};
use adv_machine::Objects;

fn contains(window: &Window, point: (i32, i32), origin: (i32, i32)) -> bool {
    let x = i64::from(point.0) - i64::from(origin.0);
    let y = i64::from(point.1) - i64::from(origin.1);
    x >= i64::from(window.rect.left)
        && x < i64::from(window.rect.right)
        && y >= i64::from(window.rect.top)
        && y < i64::from(window.rect.bottom)
}

fn transparent_static(user: &User, window: &Window) -> bool {
    // Native STATIC controls without SS_NOTIFY return HTTRANSPARENT, including
    // painted statics. SS_NOTIFY makes the control receive physical mouse input.
    user.classes[window.class]
        .name
        .eq_ignore_ascii_case("Static")
        && window.style & 0x100 == 0
}

/// Deepest eligible descendant at a screen point. Child bounds are clipped by
/// every ancestor's client area; siblings keep USER's existing top-first order.
pub(super) fn descendant(user: &User, objects: &Objects, root: u32, point: (i32, i32)) -> u32 {
    let mut current = root;
    for _ in 0..=user.windows.len() {
        let Some(window) = user.windows.get(objects, &current) else {
            return 0;
        };
        let origin = user.client_origin(objects, current);
        let (width, height) = window.client_size();
        let x = i64::from(point.0) - i64::from(origin.0);
        let y = i64::from(point.1) - i64::from(origin.1);
        if !window.enabled() || x < 0 || y < 0 || x >= i64::from(width) || y >= i64::from(height) {
            return current;
        }
        let child = window.children.iter().copied().find(|hwnd| {
            user.windows.get(objects, hwnd).is_some_and(|child| {
                child.parent == current
                    && child.visible()
                    && child.enabled()
                    && !child.destroying
                    && !transparent_static(user, child)
                    && contains(child, point, origin)
            })
        });
        let Some(child) = child else {
            return current;
        };
        current = child;
    }
    0
}

pub(super) fn from_point(user: &User, objects: &Objects, point: (i32, i32)) -> u32 {
    let root = user.top.iter().copied().find(|hwnd| {
        user.windows.get(objects, hwnd).is_some_and(|window| {
            window.visible()
                && !window.destroying
                && !transparent_static(user, window)
                && contains(window, point, (0, 0))
        })
    });
    root.map_or(0, |root| descendant(user, objects, root, point))
}

#[win32(user32, name = "WindowFromPoint", level = state)]
fn window_from_point(env: &mut Env<'_>, point: POINT) -> Handle {
    // The authored isolated-desktop oracle retains LastError, skips disabled
    // child subtrees and returns a disabled top-level window itself.
    Handle(from_point(user(env), &env.sys.objects, (point.x, point.y)))
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(window_from_point_ABI);
}

#[cfg(test)]
mod tests;
