//! user32.dll: the window model ([`window`]), messages ([`message`]), built-in controls
//! ([`controls`]) and dialogs ([`dialog`]), plus resources, input state, monitors and the
//! formatting/message-box helpers Game.exe uses.

pub(crate) mod clipboard;
pub mod compat;
pub mod controls;
pub mod dialog;
pub mod host_input;
mod input;
pub mod menu;
pub mod message;
mod resources;
pub(crate) mod strings;
pub(crate) mod system_colors;
mod system_parameters;
pub mod window;

use adv_abi::{Conv, Registry};
use adv_cpu::backend::{Backend, GuestExt, MemFault};
use adv_machine::{Env, Machine, Outcome};
use adv_pe::ResId;

pub use window::User;

use crate::printf::{StackArgs, format};
use crate::util::{cstr, put_cstr};
use crate::{Win32, stdcall};

pub const USER32: &str = "user32.dll";
/// wsprintf's documented output limit.
const WSPRINTF_MAX: u32 = 1024;

pub fn user<'a>(env: &'a Env<'_>) -> &'a User {
    &env.ext.get::<Win32>().user
}

pub fn user_mut<'a>(env: &'a mut Env<'_>) -> &'a mut User {
    &mut env.ext.get_mut::<Win32>().user
}

/// Resolve the built-in window procedures' thunks and register the system classes.
pub(crate) fn install<B: Backend>(m: &mut Machine<B>) -> Result<(), MemFault> {
    let mut proc = |name: &str| m.sys.thunks.resolve(&mut m.cpu, USER32, name);
    let procs = window::SysProcs {
        def_window: proc("DefWindowProcA")?,
        def_dlg: proc("DefDlgProcA")?,
        button: proc(controls::BUTTON_PROC)?,
        static_: proc(controls::STATIC_PROC)?,
        edit: proc(controls::EDIT_PROC)?,
        combo: proc(controls::COMBO_PROC)?,
        trackbar: proc(controls::TRACKBAR_PROC)?,
    };
    let win32 = m.ext.get_mut::<Win32>();
    win32.user.keyboard_layout = input::layout_for_codepage(win32.codec.acp);
    win32.user.add_system_classes(procs);
    Ok(())
}

fn wsprintf_a(env: &mut Env<'_>) -> Outcome {
    let (buf, fmt) = (env.arg(0), env.arg(1));
    let f = cstr(env.cpu, fmt);
    let esp = env.cpu.reg(adv_cpu::backend::Reg::Esp);
    let codec = env.ext.get::<Win32>().codec;
    let out = format(
        env.cpu,
        &codec,
        &f,
        &mut StackArgs {
            cpu: env.cpu,
            addr: esp + 12,
        },
    );
    Outcome::Return(put_cstr(env.cpu, buf, WSPRINTF_MAX, &out).unwrap_or(WSPRINTF_MAX - 1))
}

fn wvsprintf_a(env: &mut Env<'_>) -> Outcome {
    let (buf, fmt, va) = (env.arg(0), env.arg(1), env.arg(2));
    let f = cstr(env.cpu, fmt);
    let codec = env.ext.get::<Win32>().codec;
    let out = format(
        env.cpu,
        &codec,
        &f,
        &mut StackArgs {
            cpu: env.cpu,
            addr: va,
        },
    );
    Outcome::Return(put_cstr(env.cpu, buf, WSPRINTF_MAX, &out).unwrap_or(WSPRINTF_MAX - 1))
}

fn message_box_a(env: &mut Env<'_>) -> Outcome {
    let (text, caption, ty) = (env.arg(1), env.arg(2), env.arg(3));
    let c = env.ext.get::<Win32>().codec;
    let (t, cap) = (
        c.decode(&cstr(env.cpu, text)),
        c.decode(&cstr(env.cpu, caption)),
    );
    log::warn!(
        "MessageBoxA [{cap}] {t} (type {ty:#x}) from {:#010x}",
        env.caller()
    );
    // IDOK for OK-style boxes, IDYES for yes/no.
    Outcome::Return(match ty & 0xf {
        3 | 4 => 6,
        _ => 1,
    })
}

/// A single-monitor desktop of the configured size.
fn get_system_metrics(env: &mut Env<'_>) -> Outcome {
    let (w, h) = user(env).screen;
    let (w, h) = (w as u32, h as u32);
    Outcome::Return(match env.arg(0) {
        0 | 16 | 78 => w,  // SM_CXSCREEN, SM_CXFULLSCREEN, SM_CXVIRTUALSCREEN
        1 | 79 => h,       // SM_CYSCREEN, SM_CYVIRTUALSCREEN
        17 => h - 40 - 23, // SM_CYFULLSCREEN (minus taskbar and caption)
        4 => 23,           // SM_CYCAPTION
        5 | 6 => 1,        // SM_CXBORDER, SM_CYBORDER
        7 | 8 => 3,        // SM_CXFIXEDFRAME, SM_CYFIXEDFRAME
        11 | 12 => 32,     // SM_CXICON, SM_CYICON
        13 | 14 => 32,     // SM_CXCURSOR, SM_CYCURSOR
        15 => 20,          // SM_CYMENU
        23 => u32::from(user(env).swap_buttons), // SM_SWAPBUTTON
        32 | 33 => 8,      // SM_CXFRAME, SM_CYFRAME
        34 => 136,         // SM_CXMINTRACK
        35 => 39,          // SM_CYMINTRACK
        49 | 50 => 16,     // SM_CXSMICON, SM_CYSMICON
        61 | 62 => w,      // SM_CXMAXIMIZED... close enough: SM_CXMAXTRACK
        80 => 1,           // SM_CMONITORS
        n => {
            log::debug!("GetSystemMetrics({n}) = 0");
            0
        }
    })
}

fn set_rect(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    for i in 0..4 {
        let v = env.arg(1 + i);
        let _ = env.cpu.write_u32(p + 4 * i, v);
    }
    Outcome::Return(1)
}

fn set_rect_empty(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    if p != 0 {
        let _ = env.cpu.write(p, &[0u8; 16]);
    }
    Outcome::Return(1)
}

fn is_rect_empty(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let l = env.cpu.read_u32(p).unwrap_or(0) as i32;
    let t = env.cpu.read_u32(p + 4).unwrap_or(0) as i32;
    let r = env.cpu.read_u32(p + 8).unwrap_or(0) as i32;
    let b = env.cpu.read_u32(p + 12).unwrap_or(0) as i32;
    Outcome::Return(u32::from(l >= r || t >= b))
}

fn equal_rect(env: &mut Env<'_>) -> Outcome {
    let (p1, p2) = (env.arg(0), env.arg(1));
    let mut b1 = [0u8; 16];
    let mut b2 = [0u8; 16];
    let _ = env.cpu.read(p1, &mut b1);
    let _ = env.cpu.read(p2, &mut b2);
    Outcome::Return(u32::from(b1 == b2))
}

fn offset_rect(env: &mut Env<'_>) -> Outcome {
    let (p, dx, dy) = (env.arg(0), env.arg(1) as i32, env.arg(2) as i32);
    if p != 0 {
        let l = env.cpu.read_u32(p).unwrap_or(0) as i32;
        let t = env.cpu.read_u32(p + 4).unwrap_or(0) as i32;
        let r = env.cpu.read_u32(p + 8).unwrap_or(0) as i32;
        let b = env.cpu.read_u32(p + 12).unwrap_or(0) as i32;
        let _ = env.cpu.write_u32(p, (l + dx) as u32);
        let _ = env.cpu.write_u32(p + 4, (t + dy) as u32);
        let _ = env.cpu.write_u32(p + 8, (r + dx) as u32);
        let _ = env.cpu.write_u32(p + 12, (b + dy) as u32);
    }
    Outcome::Return(1)
}

fn intersect_rect(env: &mut Env<'_>) -> Outcome {
    let (dst, s1, s2) = (env.arg(0), env.arg(1), env.arg(2));
    let l1 = env.cpu.read_u32(s1).unwrap_or(0) as i32;
    let t1 = env.cpu.read_u32(s1 + 4).unwrap_or(0) as i32;
    let r1 = env.cpu.read_u32(s1 + 8).unwrap_or(0) as i32;
    let b1 = env.cpu.read_u32(s1 + 12).unwrap_or(0) as i32;

    let l2 = env.cpu.read_u32(s2).unwrap_or(0) as i32;
    let t2 = env.cpu.read_u32(s2 + 4).unwrap_or(0) as i32;
    let r2 = env.cpu.read_u32(s2 + 8).unwrap_or(0) as i32;
    let b2 = env.cpu.read_u32(s2 + 12).unwrap_or(0) as i32;

    let l = l1.max(l2);
    let t = t1.max(t2);
    let r = r1.min(r2);
    let b = b1.min(b2);

    if l < r && t < b {
        if dst != 0 {
            let _ = env.cpu.write_u32(dst, l as u32);
            let _ = env.cpu.write_u32(dst + 4, t as u32);
            let _ = env.cpu.write_u32(dst + 8, r as u32);
            let _ = env.cpu.write_u32(dst + 12, b as u32);
        }
        Outcome::Return(1)
    } else {
        if dst != 0 {
            let _ = env.cpu.write(dst, &[0u8; 16]);
        }
        Outcome::Return(0)
    }
}

fn union_rect(env: &mut Env<'_>) -> Outcome {
    let (dst, s1, s2) = (env.arg(0), env.arg(1), env.arg(2));
    let l1 = env.cpu.read_u32(s1).unwrap_or(0) as i32;
    let t1 = env.cpu.read_u32(s1 + 4).unwrap_or(0) as i32;
    let r1 = env.cpu.read_u32(s1 + 8).unwrap_or(0) as i32;
    let b1 = env.cpu.read_u32(s1 + 12).unwrap_or(0) as i32;

    let l2 = env.cpu.read_u32(s2).unwrap_or(0) as i32;
    let t2 = env.cpu.read_u32(s2 + 4).unwrap_or(0) as i32;
    let r2 = env.cpu.read_u32(s2 + 8).unwrap_or(0) as i32;
    let b2 = env.cpu.read_u32(s2 + 12).unwrap_or(0) as i32;

    let empty1 = l1 >= r1 || t1 >= b1;
    let empty2 = l2 >= r2 || t2 >= b2;

    if empty1 && empty2 {
        if dst != 0 {
            let _ = env.cpu.write(dst, &[0u8; 16]);
        }
        Outcome::Return(0)
    } else if empty1 {
        if dst != 0 {
            let mut buf = [0u8; 16];
            let _ = env.cpu.read(s2, &mut buf);
            let _ = env.cpu.write(dst, &buf);
        }
        Outcome::Return(1)
    } else if empty2 {
        if dst != 0 {
            let mut buf = [0u8; 16];
            let _ = env.cpu.read(s1, &mut buf);
            let _ = env.cpu.write(dst, &buf);
        }
        Outcome::Return(1)
    } else {
        if dst != 0 {
            let _ = env.cpu.write_u32(dst, l1.min(l2) as u32);
            let _ = env.cpu.write_u32(dst + 4, t1.min(t2) as u32);
            let _ = env.cpu.write_u32(dst + 8, r1.max(r2) as u32);
            let _ = env.cpu.write_u32(dst + 12, b1.max(b2) as u32);
        }
        Outcome::Return(1)
    }
}

fn enum_display_settings(env: &mut Env<'_>) -> Outcome {
    let (_dev, mode, out) = (env.arg(0), env.arg(1) as i32, env.arg(2));
    if (mode == 0 || mode == -1 || mode == -2) && out != 0 {
        let (w, h) = user(env).screen;
        let _ = env.cpu.write_u32(out + 40, 0x001c_0000);
        let _ = env.cpu.write_u32(out + 104, 32);
        let _ = env.cpu.write_u32(out + 108, w as u32);
        let _ = env.cpu.write_u32(out + 112, h as u32);
        let _ = env.cpu.write_u32(out + 120, 60);
        Outcome::Return(1)
    } else {
        Outcome::Return(0)
    }
}

// ---- resources ----

const RT_STRING: u16 = 6;
const RT_GROUP_CURSOR: u16 = 12;
const RT_GROUP_ICON: u16 = 14;

/// LoadCursor/LoadIcon: system ids give fixed handles; module resources a handle per name.
fn load_user_object(env: &mut Env<'_>, kind: &str, inst: u32, name: u32) -> Outcome {
    if inst == 0 {
        if name >= 0x1_0000 {
            return Outcome::Return(0);
        }
        return stock_object(env, window::SYSTEM_OBJECT_BASE + (name & 0xfff) * 4);
    }
    let id = crate::kernel32::res_id(crate::kernel32::int_or_str(env, name, false));
    let key = format!("{kind} {inst:#x} {id:?}");
    if let Some(h) = user(env).objects.find(&env.sys.objects, &key) {
        return Outcome::Return(h);
    }
    let ty = if kind == "cursor" {
        RT_GROUP_CURSOR
    } else {
        RT_GROUP_ICON
    };
    if crate::kernel32::find_resource_data(env, inst, &ResId::Id(ty), &id).is_none() {
        log::debug!("Load {kind} {id:?}: not found");
        return Outcome::Return(0);
    }
    create_user_object(env, key)
}

fn create_user_object(env: &mut Env<'_>, description: String) -> Outcome {
    match env
        .ext
        .get_mut::<Win32>()
        .user
        .objects
        .create(&mut env.sys.objects, description)
    {
        Some(handle) => Outcome::Return(handle),
        None => {
            crate::util::set_last_error(env, 8);
            Outcome::Return(0)
        }
    }
}

fn stock_object(env: &mut Env<'_>, handle: u32) -> Outcome {
    match env
        .ext
        .get_mut::<Win32>()
        .user
        .objects
        .stock(&mut env.sys.objects, handle)
    {
        Some(handle) => Outcome::Return(handle),
        None => {
            crate::util::set_last_error(env, 8);
            Outcome::Return(0)
        }
    }
}

fn load_cursor_a(env: &mut Env<'_>) -> Outcome {
    let (inst, name) = (env.arg(0), env.arg(1));
    load_user_object(env, "cursor", inst, name)
}

fn load_icon_a(env: &mut Env<'_>) -> Outcome {
    let (inst, name) = (env.arg(0), env.arg(1));
    load_user_object(env, "icon", inst, name)
}

fn load_image_a(env: &mut Env<'_>) -> Outcome {
    let (inst, name, ty) = (env.arg(0), env.arg(1), env.arg(2));
    match ty {
        1 => load_user_object(env, "icon", inst, name),
        2 => load_user_object(env, "cursor", inst, name),
        _ => {
            log::warn!("LoadImageA type {ty}: bitmaps not supported yet");
            Outcome::Return(0)
        }
    }
}

fn create_icon_indirect(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let r = |i: u32| env.cpu.read_u32(p + 4 * i).unwrap_or(0);
    let (is_icon, mask, color) = (r(0), r(3), r(4));
    let kind = if is_icon != 0 { "icon" } else { "cursor" };
    create_user_object(
        env,
        format!("{kind} indirect mask {mask:#x} color {color:#x}"),
    )
}

fn destroy_user_object(env: &mut Env<'_>) -> Outcome {
    let h = env.arg(0);
    env.ext
        .get_mut::<Win32>()
        .user
        .objects
        .remove(&mut env.sys.objects, h);
    Outcome::Return(1)
}

fn load_string_a(env: &mut Env<'_>) -> Outcome {
    let (inst, id, buf, cap) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let block = ResId::Id(((id >> 4) + 1) as u16);
    let Some((addr, _)) =
        crate::kernel32::find_resource_data(env, inst, &ResId::Id(RT_STRING), &block)
    else {
        return put_empty(env, buf, cap);
    };
    // A block holds 16 length-prefixed UTF-16 strings.
    let mut p = addr;
    for _ in 0..(id & 15) {
        let len = env.cpu.read_u16(p).unwrap_or(0);
        p += 2 + 2 * u32::from(len);
    }
    let len = env.cpu.read_u16(p).unwrap_or(0);
    if len == 0 {
        return put_empty(env, buf, cap);
    }
    let units: Vec<u16> = (0..u32::from(len))
        .map(|i| env.cpu.read_u16(p + 2 + 2 * i).unwrap_or(0))
        .collect();
    let (bytes, _) = env.ext.get::<Win32>().codec.from_utf16(0, &units);
    Outcome::Return(put_cstr(env.cpu, buf, cap, &bytes).unwrap_or(cap.saturating_sub(1)))
}

fn put_empty(env: &mut Env<'_>, buf: u32, cap: u32) -> Outcome {
    if cap > 0 {
        let _ = env.cpu.write(buf, &[0]);
    }
    Outcome::Return(0)
}

// ---- cursor and keyboard ----

fn set_cursor(env: &mut Env<'_>) -> Outcome {
    stock_object(env, window::SYSTEM_OBJECT_BASE + (32512 & 0xfff) * 4)
}

fn show_cursor(env: &mut Env<'_>) -> Outcome {
    let show = env.arg(0) != 0;
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    u.cursor_count += if show { 1 } else { -1 };
    Outcome::Return(u.cursor_count as u32)
}

fn get_cursor_pos(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let (x, y) = user(env).cursor_pos;
    let _ = env.cpu.write_u32(p, x as u32);
    let _ = env.cpu.write_u32(p + 4, y as u32);
    Outcome::Return(1)
}

fn set_cursor_pos(env: &mut Env<'_>) -> Outcome {
    let (x, y) = (env.arg(0) as i32, env.arg(1) as i32);
    user_mut(env).cursor_pos = (x, y);
    Outcome::Return(1)
}

fn get_async_key_state(env: &mut Env<'_>) -> Outcome {
    let vk = env.arg(0) as usize & 0xff;
    let frame = env.sys.frame;
    let user = user_mut(env);
    let observation = user.host_keys.as_ref().map_or_else(
        || {
            user.input_tracking
                .as_ref()
                .map_or(u64::from(user.keys[vk]), |tracking| {
                    tracking.key_observation(vk, user.keys[vk])
                })
        },
        |keys| keys.observation(vk as u32),
    );
    if let Some(tracking) = &mut user.input_tracking {
        tracking.observe_key(vk, observation, frame);
    }
    let key = observation as u8;
    Outcome::Return(if key & 0x80 != 0 { 0x8000 } else { 0 })
}

fn queued_key_state(env: &Env<'_>, vk: usize) -> u8 {
    user(env)
        .queues
        .get(&env.context)
        .map_or(0, |queue| queue.keyboard.0[vk])
}

fn get_key_state(env: &mut Env<'_>) -> Outcome {
    let vk = env.arg(0) as usize & 0xff;
    let k = queued_key_state(env, vk);
    Outcome::Return(u32::from(k & 1) | if k & 0x80 != 0 { 0xffff_8000 } else { 0 })
}

fn get_keyboard_state(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    if adv_abi::guest::check_range(env.cpu, p, 256, adv_cpu::backend::Prot::WRITE).is_err() {
        crate::util::set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let user = user(env);
    let keys = user
        .queues
        .get(&env.context)
        .map_or([0; 256], |queue| queue.keyboard.0);
    Outcome::Return(u32::from(env.cpu.write(p, &keys).is_ok()))
}

// ---- monitors and DCs ----

fn monitor_from(_env: &mut Env<'_>) -> Outcome {
    Outcome::Return(window::HMONITOR_PRIMARY)
}

fn get_monitor_info_a(env: &mut Env<'_>) -> Outcome {
    let (hmon, p) = (env.arg(0), env.arg(1));
    if hmon != window::HMONITOR_PRIMARY {
        return Outcome::Return(0);
    }
    let size = env.cpu.read_u32(p).unwrap_or(0);
    let (w, h) = user(env).screen;
    window::Rect::new(0, 0, w, h).write(env.cpu, p + 4);
    window::Rect::new(0, 0, w, h - 40).write(env.cpu, p + 20);
    let _ = env.cpu.write_u32(p + 36, 1); // MONITORINFOF_PRIMARY
    if size >= 72 {
        // MONITORINFOEXA::szDevice
        let _ = put_cstr(env.cpu, p + 40, 32, br"\\.\DISPLAY1");
    }
    Outcome::Return(1)
}

fn enum_display_monitors(env: &mut Env<'_>) -> Outcome {
    let (hdc, proc, data) = (env.arg(0), env.arg(2), env.arg(3));
    let (w, h) = user(env).screen;
    let rect = window::temp_alloc(env, 16);
    window::Rect::new(0, 0, w, h).write(env.cpu, rect);
    Outcome::call(
        proc,
        [window::HMONITOR_PRIMARY, hdc, rect, data],
        move |env, _| {
            window::temp_free(env, rect);
            Outcome::Return(1)
        },
    )
}

pub fn register(reg: &mut Registry) {
    system_colors::register(reg);
    system_parameters::register(reg);
    window::register(reg);
    message::register(reg);
    controls::register(reg);
    dialog::register(reg);
    strings::register(reg);
    menu::register(reg);
    input::register(reg);
    clipboard::register(reg);
    let s = |reg: &mut Registry, name: &str, argc: u8, h| stdcall(reg, USER32, name, argc, h);
    // WINAPIV is cdecl with a two-argument mandatory prefix, not a fixed signature.
    reg.add(
        USER32,
        "wsprintfA",
        Conv::CdeclVariadic { fixed: 2 },
        wsprintf_a,
    );
    s(reg, "wvsprintfA", 3, wvsprintf_a);
    s(reg, "MessageBoxA", 4, message_box_a);
    s(reg, "MessageBeep", 1, |_| Outcome::Return(1));
    s(reg, "GetSystemMetrics", 1, get_system_metrics);
    // The guest has one standard enhanced keyboard, independent of the host
    // language and physical device. Values are type, OEM subtype and F-key count.
    s(reg, "GetKeyboardType", 1, |e| {
        Outcome::Return(match e.arg(0) {
            0 => 4,
            1 => 0,
            2 => 12,
            _ => {
                crate::util::set_last_error(e, 87);
                0
            }
        })
    });
    s(reg, "SetRect", 5, set_rect);
    s(reg, "SetRectEmpty", 1, set_rect_empty);
    s(reg, "IsRectEmpty", 1, is_rect_empty);
    s(reg, "EqualRect", 2, equal_rect);
    s(reg, "OffsetRect", 3, offset_rect);
    s(reg, "IntersectRect", 3, intersect_rect);
    s(reg, "UnionRect", 3, union_rect);
    s(reg, "TranslateAcceleratorA", 3, |_| Outcome::Return(0));
    s(reg, "TranslateAcceleratorW", 3, |_| Outcome::Return(0));
    s(reg, "AttachThreadInput", 3, |_| Outcome::Return(1));
    s(reg, "WindowFromPoint", 2, |_| Outcome::Return(0));
    s(reg, "EnumWindows", 2, |_| Outcome::Return(1));
    s(reg, "EnumChildWindows", 3, |_| Outcome::Return(1));
    s(reg, "ChangeDisplaySettingsA", 2, |_| Outcome::Return(0));
    s(reg, "ChangeDisplaySettingsExA", 5, |_| Outcome::Return(0));
    s(reg, "ChangeDisplaySettingsW", 2, |_| Outcome::Return(0));
    s(reg, "ChangeDisplaySettingsExW", 5, |_| Outcome::Return(0));
    s(reg, "EnumDisplaySettingsA", 3, enum_display_settings);
    s(reg, "EnumDisplaySettingsW", 3, enum_display_settings);
    s(reg, "WINNLSEnableIME", 2, |_| Outcome::Return(1));
    s(reg, "LoadCursorA", 2, load_cursor_a);
    s(reg, "LoadIconA", 2, load_icon_a);
    s(reg, "LoadImageA", 6, load_image_a);
    s(reg, "CreateIconIndirect", 1, create_icon_indirect);
    s(reg, "DestroyIcon", 1, destroy_user_object);
    s(reg, "DestroyCursor", 1, destroy_user_object);
    s(reg, "LoadStringA", 4, load_string_a);
    s(reg, "SetCursor", 1, set_cursor);
    s(reg, "ShowCursor", 1, show_cursor);
    s(reg, "GetCursorPos", 1, get_cursor_pos);
    s(reg, "SetCursorPos", 2, set_cursor_pos);
    s(reg, "ClipCursor", 1, |_| Outcome::Return(1));
    s(reg, "GetAsyncKeyState", 1, get_async_key_state);
    s(reg, "GetKeyState", 1, get_key_state);
    s(reg, "GetKeyboardState", 1, get_keyboard_state);
    s(reg, "MonitorFromPoint", 3, monitor_from);
    s(reg, "MonitorFromWindow", 2, monitor_from);
    s(reg, "MonitorFromRect", 2, monitor_from);
    s(reg, "GetMonitorInfoA", 2, get_monitor_info_a);
    s(reg, "EnumDisplayMonitors", 4, enum_display_monitors);
    s(reg, "DdeInitializeA", 4, |e| {
        let p = e.arg(0);
        if p != 0 {
            let _ = e.cpu.write_u32(p, 1);
        }
        Outcome::Return(0)
    });
    s(reg, "DdeInitializeW", 4, |e| {
        let p = e.arg(0);
        if p != 0 {
            let _ = e.cpu.write_u32(p, 1);
        }
        Outcome::Return(0)
    });
    s(reg, "DdeUninitialize", 1, |_| Outcome::Return(1));
    s(reg, "DdeConnect", 4, |_| Outcome::Return(0));
    s(reg, "DdeDisconnect", 1, |_| Outcome::Return(1));
    s(reg, "DdeNameService", 4, |_| Outcome::Return(0));
    s(reg, "DdeClientTransaction", 8, |_| Outcome::Return(0));
    s(reg, "DdeCreateDataHandle", 7, |_| Outcome::Return(0));
    s(reg, "DdeGetData", 4, |_| Outcome::Return(0));
    s(reg, "DdeFreeDataHandle", 1, |_| Outcome::Return(1));
    s(reg, "DdeGetLastError", 1, |_| Outcome::Return(0));
    s(reg, "DdeCreateStringHandleA", 3, |_| Outcome::Return(1));
    s(reg, "DdeCreateStringHandleW", 3, |_| Outcome::Return(1));
    s(reg, "DdeQueryStringA", 5, |_| Outcome::Return(0));
    s(reg, "DdeQueryStringW", 5, |_| Outcome::Return(0));
    s(reg, "DdeFreeStringHandle", 2, |_| Outcome::Return(1));
    s(reg, "DdeCmpStringHandles", 2, |_| Outcome::Return(0));
}
