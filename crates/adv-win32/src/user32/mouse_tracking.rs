//! Pending USER mouse tracking belongs to the existing thread queue. No host
//! cursor or native window participates in guest hover/leave notifications.
use super::{message, user, user_mut, window::make_long};
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;

const HOVER: u32 = 1;
const LEAVE: u32 = 2;
const NONCLIENT: u32 = 0x10;
const QUERY: u32 = 0x40000000;
const CANCEL: u32 = 0x80000000;

pub(super) struct Hover {
    pub width: u32,
    pub height: u32,
    pub time: u32,
}
impl Default for Hover {
    fn default() -> Self {
        Self {
            width: 4,
            height: 4,
            time: 400,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TrackData {
    size: u32,
    flags: u32,
    hwnd: u32,
    hover_ms: u32,
}

#[derive(Clone, Copy)]
pub(super) struct Tracking {
    pub hwnd: u32,
    flags: u32,
    hover_ms: u32,
    origin: (i32, i32),
    pub due_us: Option<u64>,
}

fn inside(env: &Env<'_>, track: Tracking) -> bool {
    let u = user(env);
    let Some(w) = u
        .windows
        .get(&env.sys.objects, &track.hwnd)
        .filter(|w| w.visible() && !w.destroying)
    else {
        return false;
    };
    if super::hit_test::from_point(u, &env.sys.objects, u.cursor_pos) != track.hwnd {
        return false;
    }
    let (x, y) = u.client_origin(&env.sys.objects, track.hwnd);
    let (width, height) = w.client_size();

    i64::from(u.cursor_pos.0) >= i64::from(x)
        && i64::from(u.cursor_pos.1) >= i64::from(y)
        && i64::from(u.cursor_pos.0) < i64::from(x) + i64::from(width)
        && i64::from(u.cursor_pos.1) < i64::from(y) + i64::from(height)
}

fn leave(env: &mut Env<'_>, track: Tracking) {
    if track.flags & LEAVE != 0 {
        message::post(env, track.hwnd, 0x2a3, 0, 0);
    }
}

/// Poll before queue inspection and on physical input. Existing queue waits
/// use the same hover deadline, including when GetMessage is blocked.
pub(super) fn poll(env: &mut Env<'_>) {
    let now = env.sys.clock.now_us();
    let pending: Vec<_> = user(env)
        .queues
        .iter()
        .filter_map(|(&ctx, q)| q.mouse_tracking.map(|t| (ctx, t)))
        .collect();
    for (ctx, mut track) in pending {
        if !inside(env, track) {
            user_mut(env).queue(ctx).mouse_tracking = None;
            leave(env, track);
            continue;
        }
        let position = user(env).cursor_pos;
        if track.flags & HOVER != 0 {
            let dx = i64::from(position.0) - i64::from(track.origin.0);
            let dy = i64::from(position.1) - i64::from(track.origin.1);
            let hover = &user(env).hover;
            let (half_width, half_height) =
                (i64::from(hover.width) / 2, i64::from(hover.height) / 2);
            if !(-half_width..i64::from(hover.width) - half_width).contains(&dx)
                || !(-half_height..i64::from(hover.height) - half_height).contains(&dy)
            {
                track.origin = position;
                track.due_us = Some(now.saturating_add(u64::from(track.hover_ms) * 1000));
            }
            if track.due_us.is_some_and(|due| now >= due) {
                let u = user(env);
                let buttons = u32::from(u.keys[1] & 0x80 != 0)
                    | (u32::from(u.keys[2] & 0x80 != 0) * 2)
                    | (u32::from(u.keys[0x10] & 0x80 != 0) * 4)
                    | (u32::from(u.keys[0x11] & 0x80 != 0) * 8)
                    | (u32::from(u.keys[4] & 0x80 != 0) * 16);
                let (x, y) = u.client_origin(&env.sys.objects, track.hwnd);
                track.flags &= !HOVER;
                track.due_us = None;
                // Posting recomputes queue availability and can poll again.
                // Retire the one-shot before exposing its notification.
                user_mut(env).queue(ctx).mouse_tracking =
                    (track.flags & (HOVER | LEAVE) != 0).then_some(track);
                message::post(
                    env,
                    track.hwnd,
                    0x2a1,
                    buttons,
                    make_long(position.0 - x, position.1 - y),
                );
            }
        }
        user_mut(env).queue(ctx).mouse_tracking =
            (track.flags & (HOVER | LEAVE) != 0).then_some(track);
    }
}

#[win32(user32,name="TrackMouseEvent",level=implement)]
fn track(env: &mut Env<'_>, data: GuestPtr<[u32; 4]>) -> u32 {
    let fail = |env: &mut Env<'_>, error| {
        crate::util::set_last_error(env, error);
        0
    };
    let Ok([size, flags, hwnd, hover_ms]) = data.read(env.cpu) else {
        return fail(env, 998);
    };
    let request = TrackData {
        size,
        flags,
        hwnd,
        hover_ms,
    };
    if request.size != 16 {
        return fail(env, 87);
    }
    let ctx = env.context;
    if request.flags & QUERY != 0 {
        if check_range(env.cpu, data.addr(), 16, Prot::WRITE).is_err() {
            return fail(env, 998);
        }
        poll(env);
        let active = user(env).queues.get(&ctx).and_then(|q| q.mouse_tracking);
        let result = active.map_or(
            TrackData {
                size: 16,
                flags: 0,
                hwnd: 0,
                hover_ms: 0,
            },
            |t| TrackData {
                size: 16,
                flags: t.flags,
                hwnd: t.hwnd,
                hover_ms: t.hover_ms,
            },
        );
        return if data
            .write(
                env.cpu,
                [result.size, result.flags, result.hwnd, result.hover_ms],
            )
            .is_ok()
        {
            1
        } else {
            fail(env, 998)
        };
    }
    if request.flags & !(HOVER | LEAVE | NONCLIENT | CANCEL) != 0 {
        return fail(env, 1004);
    }
    if request.flags & NONCLIENT != 0 {
        adv_abi::warn_unavailable(
            env,
            "TrackMouseEvent",
            "nonclient tracking requires guest WM_NCHITTEST integration",
        );
        return fail(env, 50);
    }
    if !user(env)
        .windows
        .get(&env.sys.objects, &request.hwnd)
        .is_some_and(|w| !w.destroying)
    {
        return fail(env, 1400);
    }
    if request.flags & CANCEL != 0 {
        if let Some(mut active) = user(env)
            .queues
            .get(&ctx)
            .and_then(|q| q.mouse_tracking)
            .filter(|t| t.hwnd == request.hwnd && t.flags & NONCLIENT == request.flags & NONCLIENT)
        {
            active.flags &= !(request.flags & (HOVER | LEAVE));
            if active.flags & HOVER == 0 {
                active.due_us = None;
            }
            user_mut(env).queue(ctx).mouse_tracking =
                (active.flags & (HOVER | LEAVE) != 0).then_some(active);
        }
        return 1;
    }
    let now = env.sys.clock.now_us();
    let mut pending = user(env)
        .queues
        .get(&ctx)
        .and_then(|q| q.mouse_tracking)
        .filter(|t| t.hwnd == request.hwnd && t.flags & NONCLIENT == request.flags & NONCLIENT)
        .unwrap_or(Tracking {
            hwnd: request.hwnd,
            flags: request.flags & NONCLIENT,
            hover_ms: user(env).hover.time,
            origin: user(env).cursor_pos,
            due_us: None,
        });
    pending.flags |= request.flags & (HOVER | LEAVE);
    if request.flags & HOVER != 0 {
        pending.hover_ms = if request.hover_ms == u32::MAX {
            user(env).hover.time
        } else {
            request.hover_ms
        };
        pending.origin = user(env).cursor_pos;
        pending.due_us = Some(now.saturating_add(u64::from(pending.hover_ms) * 1000));
    }
    if !inside(env, pending) {
        user_mut(env).queue(ctx).mouse_tracking = None;
        leave(env, pending);
        return 1;
    }
    user_mut(env).queue(ctx).mouse_tracking =
        (pending.flags & (HOVER | LEAVE) != 0).then_some(pending);
    1
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(track_ABI);
    registry.register(adv_abi::Api {
        module: "comctl32.dll",
        name: "_TrackMouseEvent",
        ..track_ABI
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::test_support::{DATA, Fixture, env};
    use adv_cpu::backend::GuestExt;
    fn request(f: &mut Fixture, module: &str, name: &str, flags: u32, hwnd: u32, time: u32) -> u32 {
        for (index, value) in [16, flags, hwnd, time].into_iter().enumerate() {
            f.m.cpu.write_u32(DATA + index as u32 * 4, value).unwrap();
        }
        f.call(module, name, &[DATA])
    }
    #[test]
    // spec: WIN32-INPUT-1
    fn hover_moves_rearms_cancels_and_leaves_over_the_real_queue_and_clock() {
        let mut f = Fixture::new("mouse-tracking-clock");
        let hwnd = f.m.ext.get::<crate::Win32>().user.desktop;
        let u = &mut f.m.ext.get_mut::<crate::Win32>().user;
        u.top.push(hwnd);
        u.cursor_pos = (10, 10);
        assert_eq!(
            request(
                &mut f,
                "user32.dll",
                "TrackMouseEvent",
                HOVER | LEAVE,
                hwnd,
                25
            ),
            1
        );
        let initial = f.m.ext.get::<crate::Win32>().user.queues[&0]
            .mouse_tracking
            .unwrap()
            .due_us
            .unwrap();
        f.m.sys.clock.advance_to(initial - 10_000);
        f.m.ext.get_mut::<crate::Win32>().user.cursor_pos = (13, 10);
        poll(&mut env(&mut f.m));
        let reset = f.m.ext.get::<crate::Win32>().user.queues[&0]
            .mouse_tracking
            .unwrap()
            .due_us
            .unwrap();
        assert!(reset > initial);
        f.m.sys.clock.advance_to(initial + 1);
        poll(&mut env(&mut f.m));
        assert!(
            !f.m.ext.get::<crate::Win32>().user.queues[&0]
                .posted
                .iter()
                .any(|m| m.msg == 0x2a1)
        );
        f.m.sys.clock.advance_to(reset);
        poll(&mut env(&mut f.m));
        let u = &f.m.ext.get::<crate::Win32>().user;
        let hovered = u.queues[&0]
            .posted
            .iter()
            .filter(|m| m.msg == 0x2a1)
            .collect::<Vec<_>>();
        assert_eq!(hovered.len(), 1);
        assert_eq!(hovered[0].lparam, make_long(13, 10));
        assert_eq!(u.queues[&0].mouse_tracking.unwrap().flags, LEAVE);
        assert_eq!(
            request(&mut f, "comctl32.dll", "_TrackMouseEvent", HOVER, hwnd, 10),
            1
        );
        assert_eq!(
            request(
                &mut f,
                "user32.dll",
                "TrackMouseEvent",
                CANCEL | HOVER,
                hwnd,
                0
            ),
            1
        );
        assert_eq!(
            f.m.ext.get::<crate::Win32>().user.queues[&0]
                .mouse_tracking
                .unwrap()
                .flags,
            LEAVE
        );
        f.m.ext.get_mut::<crate::Win32>().user.cursor_pos = (-1, -1);
        poll(&mut env(&mut f.m));
        poll(&mut env(&mut f.m));
        let q = &f.m.ext.get::<crate::Win32>().user.queues[&0];
        assert!(q.mouse_tracking.is_none());
        assert_eq!(q.posted.iter().filter(|m| m.msg == 0x2a3).count(), 1);
        f.cleanup();
    }
    #[test]
    // spec: WIN32-INPUT-1
    fn hover_parameters_aw_feed_default_tracking_without_host_changes() {
        let mut f = Fixture::new("mouse-hover-profile");
        for api in ["SystemParametersInfoA", "SystemParametersInfoW"] {
            for (get, set, value) in [(0x62, 0x63, 8), (0x64, 0x65, 6), (0x66, 0x67, 75)] {
                assert_eq!(f.call("user32.dll", api, &[set, value, 0, 0]), 1);
                assert_eq!(f.call("user32.dll", api, &[get, 0, DATA, 0]), 1);
                assert_eq!(f.m.cpu.read_u32(DATA).unwrap(), value);
            }
        }
        let hwnd = f.m.ext.get::<crate::Win32>().user.desktop;
        let u = &mut f.m.ext.get_mut::<crate::Win32>().user;
        u.top.push(hwnd);
        u.cursor_pos = (10, 10);
        assert_eq!(
            request(
                &mut f,
                "user32.dll",
                "TrackMouseEvent",
                HOVER,
                hwnd,
                u32::MAX
            ),
            1
        );
        assert_eq!(
            request(&mut f, "user32.dll", "TrackMouseEvent", QUERY, 0, 0),
            1
        );
        assert_eq!(f.m.cpu.read_u32(DATA + 12).unwrap(), 75);
        f.cleanup();
    }
}
