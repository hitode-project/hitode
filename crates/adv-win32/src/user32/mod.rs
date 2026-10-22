//! user32.dll: the window model ([`window`]), messages ([`message`]), built-in controls
//! ([`controls`]) and dialogs ([`dialog`]), plus resources, input state, monitors and the
//! formatting/message-box helpers Game.exe uses.

mod accelerator;
mod caret;
pub(crate) mod clipboard;
pub mod compat;
pub mod controls;
pub mod dialog;
pub mod display;
mod encoding;
mod enumeration;
mod formatting;
mod geometry;
mod hit_test;
pub(crate) mod hooks;
pub mod host_input;
pub(crate) mod image;
mod input;
mod layered;
mod list_box;
pub mod menu;
pub mod message;
mod monitor;
mod mouse_tracking;
mod placement;
mod property;
mod rectangle;
mod region;
mod resources;
pub(crate) mod rich_edit;
mod scroll;
mod station;
mod status_bar;
pub(crate) mod strings;
pub(crate) mod system_colors;
mod system_parameters;
mod tab;
mod touch;
pub(crate) mod update;
pub mod window;

use adv_abi::{AnsiStr, Conv, Handle as AbiHandle, Registry, WideStr, win32};
use adv_cpu::backend::{Backend, GuestExt, MemFault};
use adv_machine::{Env, Machine, Outcome};
use adv_pe::ResId;

pub use placement::PlacementState;
pub(crate) use resources::IconImage;
pub use window::User;

use crate::util::{cstr, put_cstr};
use crate::{Win32, stdcall};

pub const USER32: &str = "user32.dll";

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
        def_window: proc(message::DEFAULT_PROC)?,
        def_dlg: proc(controls::DIALOG_PROC)?,
        button: proc(controls::BUTTON_PROC)?,
        static_: proc(controls::STATIC_PROC)?,
        edit: proc(controls::EDIT_PROC)?,
        combo: proc(controls::COMBO_PROC)?,
        trackbar: proc(controls::TRACKBAR_PROC)?,
        tab: proc(tab::PROC)?,
        status_bar: proc(status_bar::PROC)?,
        rich_edit: proc(rich_edit::PROC)?,
        list_box: proc(list_box::PROC)?,
    };
    let win32 = m.ext.get_mut::<Win32>();
    win32.user.keyboard_layout = input::layout_for_codepage(win32.codec.acp);
    win32.user.add_system_classes(procs);
    // The desktop object predates thunk installation; update its cached proc
    // along with its class so activation/owner messages reach DefWindowProc.
    if let Some(desktop) = win32
        .user
        .windows
        .get_mut(&mut m.sys.objects, &win32.user.desktop)
    {
        desktop.proc = procs.def_window;
    }
    Ok(())
}

fn message_box_a(env: &mut Env<'_>) -> Outcome {
    message_box_a_named(env, "MessageBoxA")
}

fn message_box_a_named(env: &mut Env<'_>, api: &str) -> Outcome {
    let (text, caption, ty) = (env.arg(1), env.arg(2), env.arg(3));
    let c = env.ext.get::<Win32>().codec;
    let (t, cap) = (
        c.decode_for_display(&cstr(env.cpu, text)),
        c.decode_for_display(&cstr(env.cpu, caption)),
    );
    message_box(env, env.arg(0), t, cap, ty, api)
}

fn message_box_w(env: &mut Env<'_>) -> Outcome {
    message_box_w_named(env, "MessageBoxW")
}

fn message_box_w_named(env: &mut Env<'_>, api: &str) -> Outcome {
    let t = String::from_utf16_lossy(&crate::util::wstr(env.cpu, env.arg(1)));
    let cap = String::from_utf16_lossy(&crate::util::wstr(env.cpu, env.arg(2)));
    message_box(env, env.arg(0), t, cap, env.arg(3), api)
}

// MSGBOXPARAMSA: cbSize, hwndOwner, hInstance, lpszText, lpszCaption, dwStyle,
// then icon, help and language fields. Text and caption may be string resource
// IDs of hInstance. The dialog has no custom icon or help button here.
fn message_box_indirect_a(env: &mut Env<'_>) -> Outcome {
    let params = env.arg(0);
    let mut words = [0; 6];
    for (index, word) in words.iter_mut().enumerate() {
        match env.cpu.read_u32(params.wrapping_add(4 * index as u32)) {
            Ok(value) => *word = value,
            Err(error) => return Outcome::Fail(format!("MessageBoxIndirectA parameters: {error}")),
        }
    }
    let [_, owner, instance, text, caption, style] = words;
    let string = |env: &mut Env<'_>, value: u32| {
        if value != 0 && value >> 16 == 0 {
            String::from_utf16_lossy(&resource_string(env, instance, value).unwrap_or_default())
        } else {
            let c = env.ext.get::<Win32>().codec;
            c.decode_for_display(&cstr(env.cpu, value))
        }
    };
    let (t, cap) = (string(env, text), string(env, caption));
    const MB_USERICON: u32 = 0x80;
    message_box(
        env,
        owner,
        t,
        cap,
        style & !MB_USERICON,
        "MessageBoxIndirectA",
    )
}

// Microsoft Learn: current MessageBoxEx uses the system UI language and
// otherwise behaves like MessageBox. Keep the fifth x86 stack word in its ABI.
#[win32(user32, name = "MessageBoxExA", level = state)]
fn message_box_ex_a(
    env: &mut Env<'_>,
    _owner: AbiHandle,
    _text: AnsiStr,
    _caption: AnsiStr,
    _style: u32,
    _language: u32,
) -> Outcome {
    message_box_a_named(env, "MessageBoxExA")
}
#[win32(user32, name = "MessageBoxExW", level = state)]
fn message_box_ex_w(
    env: &mut Env<'_>,
    _owner: AbiHandle,
    _text: WideStr,
    _caption: WideStr,
    _style: u32,
    _language: u32,
) -> Outcome {
    message_box_w_named(env, "MessageBoxExW")
}

fn message_box(
    env: &mut Env<'_>,
    owner: u32,
    t: String,
    cap: String,
    ty: u32,
    api: &str,
) -> Outcome {
    log::warn!(
        "{api} [{cap}] {t} (type {ty:#x}) from {:#010x}",
        env.caller()
    );
    let mut record = adv_abi::MessageBoxCall {
        sequence: 0,
        frame: env.sys.frame,
        api: format!("user32.dll!{api}"),
        caller: env.caller(),
        context: env.context,
        caption: cap,
        text: t,
        style: ty,
        result: 0,
        preceding_lookup_failure: None,
    };
    let proc = match env
        .sys
        .thunks
        .resolve(env.cpu, USER32, controls::MESSAGE_BOX_PROC)
    {
        Ok(proc) => proc,
        Err(fault) => return Outcome::Fail(format!("MessageBox procedure: {fault:?}")),
    };
    // The user answers it like any other modal dialog: the choice may decide the
    // game's window mode or settings, so nothing presses a button for them here.
    // Only an explicit headless dialog policy does, as for every dialog.
    let template = message_box_template(&record.caption, &record.text, ty);
    dialog::dialog_box_then(
        env,
        0,
        template,
        owner,
        proc,
        0,
        true,
        Box::new(move |env, result| {
            let result = if result == u32::MAX { 0 } else { result };
            record.result = result;
            env.ext
                .get_or_default::<adv_abi::ApiDiagnostics>()
                .record_message_box(record);
            Outcome::Return(result)
        }),
    )
}

/// MessageBox buttons and their command IDs, by `MB_TYPEMASK`.
fn message_box_buttons(ty: u32) -> &'static [(u32, &'static str)] {
    const OK: (u32, &str) = (1, "OK");
    const CANCEL: (u32, &str) = (2, "Cancel");
    const ABORT: (u32, &str) = (3, "Abort");
    const RETRY: (u32, &str) = (4, "Retry");
    const IGNORE: (u32, &str) = (5, "Ignore");
    const YES: (u32, &str) = (6, "Yes");
    const NO: (u32, &str) = (7, "No");
    const TRY_AGAIN: (u32, &str) = (10, "Try Again");
    const CONTINUE: (u32, &str) = (11, "Continue");
    match ty & 0xf {
        1 => &[OK, CANCEL],
        2 => &[ABORT, RETRY, IGNORE],
        3 => &[YES, NO, CANCEL],
        4 => &[YES, NO],
        5 => &[RETRY, CANCEL],
        6 => &[CANCEL, TRY_AGAIN, CONTINUE],
        _ => &[OK],
    }
}

/// A dialog template laid out like the system message box: wrapped text above a
/// centered row of push buttons, the `MB_DEFBUTTONn` one being the default.
fn message_box_template(caption: &str, text: &str, ty: u32) -> dialog::Template {
    const MARGIN: i16 = 10;
    const WRAP: i32 = 280;
    const BUTTON: (i16, i16) = (50, 14);
    const GAP: i16 = 6;
    // Dialog units: an average character is 4 wide, a full-width one 8; a line is 8
    // high, with a little leading for full-width glyphs.
    let (mut width, mut rows) = (0, 0);
    for line in text.split('\n') {
        let line_width: i32 = line
            .trim_end_matches('\r')
            .chars()
            .map(|c| if c.is_ascii() { 4 } else { 8 })
            .sum();
        width = width.max(line_width.min(WRAP));
        rows += (line_width + WRAP - 1).max(WRAP) / WRAP;
    }
    let buttons = message_box_buttons(ty);
    let default = ((ty >> 8) & 0xf) as usize;
    let default = if default < buttons.len() { default } else { 0 };
    let row = buttons.len() as i16 * (BUTTON.0 + GAP) - GAP;
    let text_size = (width as i16, rows as i16 * 10);
    let cx = text_size.0.max(row).max(120) + 2 * MARGIN;
    let cy = MARGIN + text_size.1 + MARGIN + BUTTON.1 + 7;
    let mut items = vec![dialog::Item {
        // SS_LEFT | SS_NOPREFIX
        style: 0x5000_0080,
        ex_style: 0,
        x: MARGIN,
        y: MARGIN,
        cx: cx - 2 * MARGIN,
        cy: text_size.1,
        id: 0xffff,
        class: dialog::SzOrOrd::Ord(0x82),
        title: dialog::SzOrOrd::Str(text.replace("\r\n", "\n").replace('\n', "\r\n")),
        data: None,
    }];
    let mut x = (cx - row) / 2;
    for (index, &(id, label)) in buttons.iter().enumerate() {
        items.push(dialog::Item {
            // WS_CHILD | WS_VISIBLE | WS_TABSTOP, BS_DEFPUSHBUTTON for the default.
            style: 0x5001_0000 | u32::from(index == default),
            ex_style: 0,
            x,
            y: cy - 7 - BUTTON.1,
            cx: BUTTON.0,
            cy: BUTTON.1,
            id,
            class: dialog::SzOrOrd::Ord(0x80),
            title: dialog::SzOrOrd::Str(label.into()),
            data: None,
        });
        x += BUTTON.0 + GAP;
    }
    // Default buttons first in tab order, so the initial focus is the default.
    items[1..].rotate_left(default);
    dialog::Template {
        // WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_CENTER | DS_MODALFRAME | DS_SETFONT
        style: 0x80c8_08c0,
        // WS_EX_TOPMOST for MB_SYSTEMMODAL and MB_TOPMOST.
        ex_style: if ty & 0x4_1000 != 0 { 0x8 } else { 0 },
        x: 0,
        y: 0,
        cx,
        cy,
        menu: dialog::SzOrOrd::None,
        class: dialog::SzOrOrd::None,
        title: caption.into(),
        font: Some((9, "MS Shell Dlg".into())),
        items,
    }
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
        36 => user(env).double_click.width, // SM_CXDOUBLECLK
        37 => user(env).double_click.height, // SM_CYDOUBLECLK
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
    if p == 0 {
        return Outcome::Return(0);
    }
    let _ = env.cpu.write(p, &[0u8; 16]);
    Outcome::Return(1)
}

fn is_rect_empty(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    if p == 0 {
        return Outcome::Return(1);
    }
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
    if dst == 0 || s1 == 0 || s2 == 0 {
        return Outcome::Return(0);
    }
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
    if dst == 0 || s1 == 0 || s2 == 0 {
        return Outcome::Return(0);
    }
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
        // One USER owner, with separate fixed stock identity ranges. Preserve
        // the existing cursor values (also used by cursor_handle defaults).
        let kind_offset = if kind == "icon" { 0x4000 } else { 0 };
        return stock_object(
            env,
            window::SYSTEM_OBJECT_BASE + kind_offset + (name & 0xfff) * 4,
        );
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
    Outcome::Return(image::load_cursor(env, inst, name, false).0)
}

#[win32(user32, name = "LoadCursorW", level = implement)]
fn load_cursor_w(env: &mut Env<'_>, instance: AbiHandle, name: WideStr) -> AbiHandle {
    image::load_cursor(env, instance.0, name.0.addr(), true)
}

fn load_cursor_from_file_a(env: &mut Env<'_>) -> Outcome {
    let filename_ptr = env.arg(0);
    if filename_ptr == 0 {
        crate::util::set_last_error(env, 87);
        return Outcome::Return(0);
    }
    stock_object(env, window::SYSTEM_OBJECT_BASE + (32512 & 0xfff) * 4)
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
    let units = resource_string(env, inst, id).unwrap_or_default();
    if units.is_empty() {
        return put_empty(env, buf, cap);
    }
    let (bytes, _) = env.ext.get::<Win32>().codec.from_utf16(0, &units);
    Outcome::Return(put_cstr(env.cpu, buf, cap, &bytes).unwrap_or(cap.saturating_sub(1)))
}

/// String `id` of a module's RT_STRING table: a block holds 16 length-prefixed
/// UTF-16 strings.
fn resource_string(env: &mut Env<'_>, instance: u32, id: u32) -> Option<Vec<u16>> {
    let block = ResId::Id(((id >> 4) + 1) as u16);
    let (addr, _) =
        crate::kernel32::find_resource_data(env, instance, &ResId::Id(RT_STRING), &block)?;
    let mut p = addr;
    for _ in 0..(id & 15) {
        let len = env.cpu.read_u16(p).unwrap_or(0);
        p += 2 + 2 * u32::from(len);
    }
    let len = env.cpu.read_u16(p).unwrap_or(0);
    Some(
        (0..u32::from(len))
            .map(|i| env.cpu.read_u16(p + 2 + 2 * i).unwrap_or(0))
            .collect(),
    )
}

#[adv_abi::win32(user32, name = "LoadStringW", level = implement)]
fn load_string_w(
    env: &mut Env<'_>,
    instance: adv_abi::Handle,
    id: u32,
    output: adv_abi::WideStr,
    capacity: i32,
) -> Outcome {
    let missing = |env: &mut Env<'_>| {
        if capacity > 0 {
            let _ = env.cpu.write(output.0.addr(), &[0, 0]);
        }
        Outcome::Return(0)
    };
    if capacity < 0 {
        return missing(env);
    }
    let block = ResId::Id(((id >> 4) + 1) as u16);
    let Some((address, size)) =
        crate::kernel32::find_resource_data(env, instance.0, &ResId::Id(RT_STRING), &block)
    else {
        return missing(env);
    };
    let Some(end) = address.checked_add(size) else {
        return missing(env);
    };
    let mut pointer = address;
    for index in 0..=(id & 15) {
        if pointer.checked_add(2).is_none_or(|next| next > end) {
            return missing(env);
        }
        let Ok(length) = env.cpu.read_u16(pointer) else {
            return missing(env);
        };
        pointer += 2;
        let next = pointer.checked_add(u32::from(length) * 2);
        if next.is_none_or(|next| next > end) {
            return missing(env);
        }
        if index != id & 15 {
            pointer = next.unwrap();
            continue;
        }
        if capacity == 0 {
            return Outcome::Return(if env.cpu.write_u32(output.0.addr(), pointer).is_ok() {
                u32::from(length)
            } else {
                0
            });
        }
        let count = u32::from(length).min(capacity as u32 - 1);
        let mut bytes = vec![0; count as usize * 2 + 2];
        if env
            .cpu
            .read(pointer, &mut bytes[..count as usize * 2])
            .is_err()
            || env.cpu.write(output.0.addr(), &bytes).is_err()
        {
            return Outcome::Return(0);
        }
        return Outcome::Return(count);
    }
    missing(env)
}

fn put_empty(env: &mut Env<'_>, buf: u32, cap: u32) -> Outcome {
    if cap > 0 {
        let _ = env.cpu.write(buf, &[0]);
    }
    Outcome::Return(0)
}

// ---- cursor and keyboard ----

fn set_cursor(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    if handle != 0 && !user(env).objects.contains(&env.sys.objects, handle) {
        crate::util::set_last_error(env, 1402); // ERROR_INVALID_CURSOR_HANDLE
        return Outcome::Return(0);
    }
    Outcome::Return(std::mem::replace(&mut user_mut(env).cursor_handle, handle))
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
    input::set_cursor_position(user_mut(env), (x, y));
    Outcome::Return(1)
}

#[win32(user32, name = "GetAsyncKeyState", level = implement, leaf)]
fn get_async_key_state(env: &mut Env<'_>, vk: i32) -> u32 {
    let vk = vk as usize & 0xff;
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
    if key & 0x80 != 0 { 0x8000 } else { 0 }
}

fn queued_key_state(env: &Env<'_>, vk: usize) -> u8 {
    user(env)
        .queues
        .get(&env.context)
        .map_or(0, |queue| queue.keyboard.0[vk])
}

#[win32(user32, name = "GetKeyState", level = implement, leaf)]
fn get_key_state(env: &mut Env<'_>, vk: i32) -> u32 {
    let vk = vk as usize & 0xff;
    let k = queued_key_state(env, vk);
    u32::from(k & 1) | if k & 0x80 != 0 { 0xffff_8000 } else { 0 }
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

fn keyboard_leaf(env: &mut Env<'_>) -> Option<u64> {
    let p = env.cpu.arg(0).ok()?;
    adv_abi::guest::check_range(env.cpu, p, 256, adv_cpu::backend::Prot::WRITE).ok()?;
    let keys = user(env)
        .queues
        .get(&env.context)
        .map_or([0; 256], |queue| queue.keyboard.0);
    env.cpu.write(p, &keys).ok()?;
    Some((u64::from(adv_abi::leaf_edx(env)) << 32) | 1)
}

// ---- monitors and DCs ----

fn monitor_from(_env: &mut Env<'_>) -> Outcome {
    Outcome::Return(window::HMONITOR_PRIMARY)
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
    rich_edit::register(reg);
    update::register(reg);
    formatting::register(reg);
    hooks::register(reg);
    monitor::register(reg);
    accelerator::register(reg);
    station::register(reg);
    system_colors::register(reg);
    system_parameters::register(reg);
    window::register(reg);
    touch::register(reg);
    encoding::register(reg);
    placement::register(reg);
    property::register(reg);
    rectangle::register(reg);
    region::register(reg);
    message::register(reg);
    controls::register(reg);
    dialog::register(reg);
    strings::register(reg);
    scroll::register(reg);
    layered::register(reg);
    enumeration::register(reg);
    menu::register(reg);
    input::register(reg);
    mouse_tracking::register(reg);
    caret::register(reg);
    image::register(reg);
    clipboard::register(reg);
    let s = |reg: &mut Registry, name: &str, argc: u8, h| stdcall(reg, USER32, name, argc, h);
    s(reg, "MessageBoxA", 4, message_box_a);
    s(reg, "MessageBoxW", 4, message_box_w);
    s(reg, "MessageBoxIndirectA", 1, message_box_indirect_a);
    reg.register(message_box_ex_a_ABI);
    reg.register(message_box_ex_w_ABI);
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
    s(reg, "AttachThreadInput", 3, |_| Outcome::Return(1));
    hit_test::register(reg);
    geometry::register(reg);
    s(reg, "EnumWindows", 2, |_| Outcome::Return(1));
    s(reg, "EnumChildWindows", 3, |_| Outcome::Return(1));
    s(reg, "ChangeDisplaySettingsA", 2, |_| Outcome::Return(0));
    s(reg, "ChangeDisplaySettingsExA", 5, |_| Outcome::Return(0));
    s(reg, "ChangeDisplaySettingsW", 2, |_| Outcome::Return(0));
    s(reg, "ChangeDisplaySettingsExW", 5, |_| Outcome::Return(0));
    s(reg, "WINNLSEnableIME", 2, |_| Outcome::Return(1));
    s(reg, "LoadCursorA", 2, load_cursor_a);
    reg.register(load_cursor_w_ABI);
    s(reg, "LoadCursorFromFileA", 1, load_cursor_from_file_a);
    s(reg, "LoadCursorFromFileW", 1, load_cursor_from_file_a);
    s(reg, "CreateIconIndirect", 1, create_icon_indirect);
    s(reg, "DestroyIcon", 1, destroy_user_object);
    s(reg, "DestroyCursor", 1, destroy_user_object);
    s(reg, "LoadStringA", 4, load_string_a);
    reg.register(load_string_w_ABI);
    s(reg, "SetCursor", 1, set_cursor);
    s(reg, "ShowCursor", 1, show_cursor);
    s(reg, "GetCursorPos", 1, get_cursor_pos);
    s(reg, "SetCursorPos", 2, set_cursor_pos);
    s(reg, "EnableScrollBar", 3, |_| Outcome::Return(1));
    s(reg, "ShowScrollBar", 3, |_| Outcome::Return(1));
    reg.register(get_async_key_state_ABI);
    reg.register(get_key_state_ABI);
    reg.add_env_leaf(
        USER32,
        "GetKeyboardState",
        Conv::Stdcall { argc: 1 },
        get_keyboard_state,
        keyboard_leaf,
    );
    s(reg, "MonitorFromPoint", 3, monitor_from);
    s(reg, "MonitorFromWindow", 2, monitor_from);
    s(reg, "MonitorFromRect", 2, monitor_from);
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
