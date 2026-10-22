//! Point mapping and immediate-child lookup use the shared USER geometry owner.
use super::{
    User, user,
    window::{WS_EX_LAYOUTRTL, make_long},
};
use crate::util::set_last_error;
use adv_abi::{Env, GuestPtr, Handle, Registry, types::POINT, win32};
use adv_cpu::backend::{GuestExt, Prot};

fn origin(u: &User, objects: &adv_machine::Objects, hwnd: u32) -> Option<(i32, i32, bool)> {
    if hwnd == 0 {
        return Some((0, 0, false));
    }
    let window = u.windows.get(objects, &hwnd)?;
    if window.destroying {
        return None;
    }
    let (x, y) = u.client_origin(objects, hwnd);
    let mirrored = window.ex_style & WS_EX_LAYOUTRTL != 0;
    Some((
        if mirrored {
            x.wrapping_add(window.client_size().0)
        } else {
            x
        },
        y,
        mirrored,
    ))
}
pub(super) fn client_point(
    u: &User,
    objects: &adv_machine::Objects,
    hwnd: u32,
    screen: (i32, i32),
) -> Option<(i32, i32)> {
    let (x, y, mirrored) = origin(u, objects, hwnd)?;
    Some((
        if mirrored {
            x.wrapping_sub(screen.0)
        } else {
            screen.0.wrapping_sub(x)
        },
        screen.1.wrapping_sub(y),
    ))
}
#[win32(user32, name = "MapWindowPoints", level = state)]
fn map(env: &mut Env<'_>, from: Handle, to: Handle, points: GuestPtr<POINT>, count: u32) -> i32 {
    let (Some((fx, fy, fr)), Some((tx, ty, tr))) = (
        origin(user(env), &env.sys.objects, from.0),
        origin(user(env), &env.sys.objects, to.0),
    ) else {
        set_last_error(env, 1400);
        return 0;
    };
    let Some(bytes) = count.checked_mul(8) else {
        set_last_error(env, 87);
        return 0;
    };
    if count != 0
        && (points.is_null()
            || adv_abi::guest::check_range(env.cpu, points.addr(), bytes, Prot::READ | Prot::WRITE)
                .is_err())
    {
        set_last_error(env, 998);
        return 0;
    }
    let mut dx = fx.wrapping_sub(tx);
    if fr {
        dx = dx.wrapping_neg();
    }
    let dy = fy.wrapping_sub(ty);
    for index in 0..count {
        let address = points.addr() + index * 8;
        let mut x = (env.cpu.read_u32(address).unwrap() as i32).wrapping_add(dx);
        let y = (env.cpu.read_u32(address + 4).unwrap() as i32).wrapping_add(dy);
        if fr != tr {
            x = x.wrapping_neg();
        }
        env.cpu.write_u32(address, x as u32).unwrap();
        env.cpu.write_u32(address + 4, y as u32).unwrap();
    }
    if count == 2 && (fr || tr) {
        let left = env.cpu.read_u32(points.addr()).unwrap() as i32;
        let right = env.cpu.read_u32(points.addr() + 8).unwrap() as i32;
        if left > right {
            env.cpu.write_u32(points.addr(), right as u32).unwrap();
            env.cpu.write_u32(points.addr() + 8, left as u32).unwrap();
        }
    }
    make_long(dx, dy) as i32
}
#[win32(user32, name = "ChildWindowFromPoint", level = state)]
fn child(env: &mut Env<'_>, parent: Handle, point: POINT) -> Handle {
    let u = user(env);
    let Some(window) = u
        .windows
        .get(&env.sys.objects, &parent.0)
        .filter(|w| !w.destroying)
    else {
        set_last_error(env, 1400);
        return Handle(0);
    };
    let (width, height) = window.client_size();
    if point.x < 0 || point.y < 0 || point.x >= width || point.y >= height {
        return Handle(0);
    }
    let children = if parent.0 == u.desktop {
        &u.top
    } else {
        &window.children
    };
    Handle(
        children
            .iter()
            .copied()
            .find(|hwnd| {
                u.windows.get(&env.sys.objects, hwnd).is_some_and(|w| {
                    !w.destroying
                        && point.x >= w.rect.left
                        && point.x < w.rect.right
                        && point.y >= w.rect.top
                        && point.y < w.rect.bottom
                })
            })
            .unwrap_or(parent.0),
    )
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(map_ABI);
    registry.register(child_ABI);
}
