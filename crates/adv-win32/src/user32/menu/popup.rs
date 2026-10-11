//! USER-owned popup tracking; the frontend presents immutable menu data and
//! returns a validated choice, never native handles or an automatic selection.
use super::*;
#[cfg(test)]
mod tests;
use adv_abi::{GuestPtr, guest::check_range};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) struct Tracking {
    pub owner: u32,
    pub root: u32,
    pub context: u32,
    pub flags: u32,
    pub x: i32,
    pub y: i32,
    pub exclude: Option<[i32; 4]>,
    pub ended: bool,
    pub ready: bool,
    pub choice: Option<MenuCommand>,
    failed: bool,
    pub active: Arc<AtomicBool>,
    highlight: Option<usize>,
}
impl Drop for Tracking {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}
pub(super) fn cancel(env: &mut Env<'_>) {
    let context = env.context;
    let menus = &mut user_mut(env).menus;
    if let Some(t) = menus.tracking.as_mut().filter(|t| t.context == context) {
        t.ended = true;
        t.active.store(false, Ordering::Release);
        menus.revision = menus.revision.wrapping_add(1);
    }
}
pub(super) fn select(env: &mut Env<'_>, command: MenuCommand) -> bool {
    let menus = &mut user_mut(env).menus;
    let Some(t) = menus
        .tracking
        .as_mut()
        .filter(|t| !t.ended && t.root == command.root && t.owner == command.window)
    else {
        return false;
    };
    t.choice = Some(command);
    t.ended = true;
    t.active.store(false, Ordering::Release);
    menus.revision = menus.revision.wrapping_add(1);
    super::super::message::post(env, command.window, 0, 0, 0)
}
fn choices(env: &Env<'_>) -> Vec<MenuCommand> {
    fn collect(items: &[MenuItem], enabled: bool, result: &mut Vec<MenuCommand>) {
        for item in items {
            let enabled = enabled && item.supported && item.flags & (3 | 0x800) == 0;
            if enabled && item.submenu == 0 {
                result.push(item.command);
            } else if enabled {
                collect(&item.children, enabled, result);
            }
        }
    }
    let mut result = Vec::new();
    if let Ok(bar) = presentation(user(env), &env.sys.objects) {
        collect(&bar.items, true, &mut result);
    }
    result
}
fn finish(env: &mut Env<'_>) -> Outcome {
    let Some(t) = user_mut(env).menus.tracking.take() else {
        return Outcome::Return(0);
    };
    user_mut(env).menus.revision = user(env).menus.revision.wrapping_add(1);
    t.active.store(false, Ordering::Release);
    let owner = t.owner;
    let root = t.root;
    let flags = t.flags;
    let choice = t.choice;
    let failed = t.failed;
    let mut notifications = VecDeque::new();
    if let Some(command) = choice {
        let item_flags = user(env)
            .menus
            .get(&env.sys.objects, command.menu)
            .and_then(|items| items.get(command.position as usize))
            .map_or(0, |i| i.flags);
        notifications.push_back([
            owner,
            0x11f,
            (command.id & 0xffff) | ((item_flags | 0x80) << 16),
            command.menu,
        ]);
    }
    if flags & 0x80 == 0 {
        notifications.push_back([owner, 0x125, root, 0]);
    }
    // Native NONOTIFY still reports menu selection/closure. It suppresses
    // loop/init/uninit notifications; RETURNCMD suppresses WM_COMMAND.
    notifications.push_back([owner, 0x11f, 0xffff_0000, 0]);
    if flags & 0x80 == 0 {
        notifications.push_back([owner, 0x212, 1, 0]);
    }
    if !failed && flags & 0x100 == 0 {
        if let Some(command) = choice {
            notifications.push_back([owner, 0x111, command.id & 0xffff, 0]);
        }
    }
    super::super::message::send_all(env, notifications, move |_env| {
        if failed {
            return Outcome::Return(0);
        }
        if flags & 0x100 == 0 {
            Outcome::Return(1)
        } else {
            Outcome::Return(choice.map_or(0, |command| command.id))
        }
    })
}
fn pump(env: &mut Env<'_>) -> Outcome {
    super::super::message::sent::drain(env, pump_inner)
}
fn pump_inner(env: &mut Env<'_>) -> Outcome {
    let Some(t) = user(env).menus.tracking.as_ref() else {
        return Outcome::Return(0);
    };
    if t.ended
        || !user(env).is_window(&env.sys.objects, t.owner)
        || user(env).menus.get(&env.sys.objects, t.root).is_none()
    {
        return finish(env);
    }
    if let Some(m) = super::super::message::next_any(env) {
        if m.msg == super::super::window::wm::QUIT {
            let ctx = env.context;
            user_mut(env).queue(ctx).quit = Some(m.wparam);
            cancel(env);
            return finish(env);
        }
        if m.msg == 0x100 && m.wparam == 27 {
            cancel(env);
            return finish(env);
        }
        if m.msg == 0x100 && matches!(m.wparam, 13 | 38 | 40) {
            let items = choices(env);
            if !items.is_empty() {
                let t = user_mut(env).menus.tracking.as_mut().unwrap();
                let index = match (t.highlight, m.wparam) {
                    (None, 38) => items.len() - 1,
                    (None, _) => 0,
                    (Some(i), 40) => (i + 1) % items.len(),
                    (Some(i), 38) => (i + items.len() - 1) % items.len(),
                    (Some(i), _) => i % items.len(),
                };
                t.highlight = Some(index);
                let command = items[index];
                if m.wparam == 13 {
                    select(env, command);
                    return finish(env);
                }
            }
            return pump(env);
        }
        if m.msg == 0x1f {
            cancel(env);
            return finish(env);
        }
        return super::super::message::dispatch(env, m, |env, _| pump(env));
    }
    super::super::message::wait_for_message(env, pump)
}
fn track(
    env: &mut Env<'_>,
    menu: u32,
    flags: u32,
    x: i32,
    y: i32,
    owner: u32,
    exclude: Option<[i32; 4]>,
) -> Outcome {
    if !user(env).is_window(&env.sys.objects, owner) {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    }
    if user(env).menus.get(&env.sys.objects, menu).is_none() {
        set_last_error(env, 1401);
        return Outcome::Return(0);
    }
    if user(env).menus.tracking.is_some() {
        set_last_error(env, 1446);
        return Outcome::Return(0);
    }
    if flags & !0x1fdfe != 0 {
        adv_abi::warn_unavailable(
            env,
            "TrackPopupMenu",
            "recursive tracking or unknown menu flags are unsupported",
        );
        set_last_error(env, 50);
        return Outcome::Return(0);
    }
    if flags & 0xc == 0xc || flags & 0x30 == 0x30 {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let owner_thread = user(env)
        .windows
        .get(&env.sys.objects, &owner)
        .unwrap()
        .thread;
    if owner_thread != env.context {
        adv_abi::warn_unavailable(
            env,
            "TrackPopupMenu",
            "cross-thread menu owners require a separate tracking queue",
        );
        set_last_error(env, 50);
        return Outcome::Return(0);
    }
    let context = env.context;
    let menus = &mut user_mut(env).menus;
    menus.tracking = Some(Tracking {
        owner,
        root: menu,
        context,
        flags,
        x,
        y,
        exclude,
        ended: false,
        ready: false,
        choice: None,
        failed: false,
        active: Arc::new(AtomicBool::new(true)),
        highlight: None,
    });
    menus.revision = menus.revision.wrapping_add(1);
    set_last_error(env, 0);
    let mut notifications = VecDeque::new();
    if flags & 0x80 == 0 {
        notifications.extend([
            [owner, 0x211, 1, 0],
            [owner, 0x116, menu, 0],
            [owner, 0x117, menu, 0],
        ]);
    }
    super::super::message::send_all(env, notifications, |env| {
        // Initialization callbacks can change or destroy the live menu.
        if let Some(t) = user_mut(env).menus.tracking.as_mut() {
            t.ready = true;
        }
        user_mut(env).menus.revision = user(env).menus.revision.wrapping_add(1);
        fn supported(items: &[MenuItem]) -> bool {
            items
                .iter()
                .all(|i| (i.supported || i.flags & (3 | 0x800) != 0) && supported(&i.children))
        }
        let error = match presentation(user(env), &env.sys.objects) {
            Ok(bar) if supported(&bar.items) => None,
            Ok(_) => Some("owner-drawn and bitmap menu items have no presentation backend".into()),
            Err(error) => Some(error),
        };
        if let Some(error) = error {
            adv_abi::warn_unavailable(env, "TrackPopupMenu", &error);
            set_last_error(env, 50);
            if let Some(tracking) = user_mut(env).menus.tracking.as_mut() {
                tracking.failed = true;
            }
            cancel(env);
        }
        pump(env)
    })
}
#[win32(user32,name="TrackPopupMenu",level=state)]
fn track_menu(
    env: &mut Env<'_>,
    menu: Handle,
    flags: u32,
    x: i32,
    y: i32,
    reserved: i32,
    owner: Handle,
    _rect: GuestPtr<()>,
) -> Outcome {
    if reserved != 0 {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    track(env, menu.0, flags, x, y, owner.0, None)
}
#[win32(user32,name="TrackPopupMenuEx",level=state)]
fn track_menu_ex(
    env: &mut Env<'_>,
    menu: Handle,
    flags: u32,
    x: i32,
    y: i32,
    owner: Handle,
    params: GuestPtr<()>,
) -> Outcome {
    let exclude = if params.is_null() {
        None
    } else {
        if check_range(env.cpu, params.addr(), 20, Prot::READ).is_err()
            || env.cpu.read_u32(params.addr()).ok() != Some(20)
        {
            set_last_error(env, 87);
            return Outcome::Return(0);
        }
        let mut rect = [0; 4];
        for (i, value) in rect.iter_mut().enumerate() {
            *value = env.cpu.read_u32(params.addr() + 4 + i as u32 * 4).unwrap() as i32;
        }
        Some(rect)
    };
    track(env, menu.0, flags, x, y, owner.0, exclude)
}
#[win32(user32,name="EndMenu",level=state)]
fn end_menu(env: &mut Env<'_>) -> i32 {
    cancel(env);
    1
}
pub(super) fn register(reg: &mut Registry) {
    for api in [track_menu_ABI, track_menu_ex_ABI, end_menu_ABI] {
        reg.register(api);
    }
}
