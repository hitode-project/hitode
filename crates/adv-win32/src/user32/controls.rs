//! Built-in window classes (BUTTON, STATIC, EDIT) and the dialog procedure DefDlgProc.
//! They are host functions reached through thunks, like any window procedure, so guest
//! subclassing (SetWindowLong(GWL_WNDPROC) + CallWindowProc) works on them.

use adv_abi::{Handle, Registry, win32};
use adv_machine::{Env, Outcome};

use super::message::{call_proc, def_window_proc, post, send, validate};
use super::window::{DWL_MSGRESULT, make_long, wm};
use super::{USER32, user, user_mut};
use crate::stdcall;

pub const BUTTON_PROC: &str = "<ButtonWndProc>";
pub const STATIC_PROC: &str = "<StaticWndProc>";
pub const EDIT_PROC: &str = "<EditWndProc>";
pub const COMBO_PROC: &str = "<ComboWndProc>";
pub const TRACKBAR_PROC: &str = "<TrackbarWndProc>";
pub const DIALOG_PROC: &str = "<DialogWndProc>";

const BM_GETCHECK: u32 = 0xf0;
const BM_SETCHECK: u32 = 0xf1;
const BM_GETSTATE: u32 = 0xf2;
const BM_SETSTATE: u32 = 0xf3;
const BM_SETSTYLE: u32 = 0xf4;
const BM_CLICK: u32 = 0xf5;
const BST_PUSHED: u32 = 0x4;
const BN_CLICKED: u32 = 0;

const BS_AUTOCHECKBOX: u32 = 3;
const BS_AUTO3STATE: u32 = 6;
const BS_AUTORADIOBUTTON: u32 = 9;
const WS_GROUP: u32 = 0x0002_0000;

fn args(env: &Env<'_>) -> (u32, u32, u32, u32) {
    (env.arg(0), env.arg(1), env.arg(2), env.arg(3))
}

fn button_proc(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = args(env);
    let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        return Outcome::Return(0);
    };
    let kind = w.style & 0xf;
    let (id, parent) = (w.id, w.parent);
    let r = match msg {
        BM_GETCHECK => w.control.button & 3,
        BM_GETSTATE => w.control.button,
        BM_SETCHECK => {
            set_button(env, hwnd, |s| (s & !3) | (wp & 3));
            0
        }
        BM_SETSTATE => {
            set_button(env, hwnd, |s| {
                if wp != 0 {
                    s | BST_PUSHED
                } else {
                    s & !BST_PUSHED
                }
            });
            0
        }
        BM_SETSTYLE => {
            if let Some(w) = env
                .ext
                .get::<crate::Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &hwnd)
            {
                w.style = (w.style & !0xffff) | (wp & 0xffff);
            }
            0
        }
        BM_CLICK => {
            // BM_CLICK finishes the simulated click (and BN_CLICKED) before returning.
            return send(env, hwnd, wm::LBUTTONDOWN, 1, 0, move |env, _| {
                send(env, hwnd, wm::LBUTTONUP, 0, 0, |_, _| Outcome::Return(0))
            });
        }
        wm::LBUTTONDOWN => {
            set_button(env, hwnd, |s| s | BST_PUSHED);
            let ctx = env.context;
            user_mut(env).queue(ctx).capture = hwnd;
            0
        }
        wm::LBUTTONUP => {
            let pushed = w.control.button & BST_PUSHED != 0;
            set_button(env, hwnd, |s| s & !BST_PUSHED);
            let ctx = env.context;
            let q = user_mut(env).queue(ctx);
            if q.capture == hwnd {
                q.capture = 0;
            }
            if !pushed {
                return Outcome::Return(0);
            }
            match kind {
                BS_AUTOCHECKBOX => set_button(env, hwnd, |s| s ^ 1),
                BS_AUTO3STATE => set_button(env, hwnd, |s| (s & !3) | (((s & 3) + 1) % 3)),
                BS_AUTORADIOBUTTON => check_radio(env, hwnd),
                _ => {}
            }
            // The parent learns about the click through WM_COMMAND.
            return send(
                env,
                parent,
                wm::COMMAND,
                make_long(id as i32, BN_CLICKED as i32),
                hwnd,
                |_, _| Outcome::Return(0),
            );
        }
        wm::GETDLGCODE => match kind {
            0 => 0x2010,                  // DLGC_BUTTON | DLGC_UNDEFPUSHBUTTON
            1 => 0x2020,                  // DLGC_BUTTON | DLGC_DEFPUSHBUTTON
            BS_AUTORADIOBUTTON => 0x2040, // DLGC_BUTTON | DLGC_RADIOBUTTON
            _ => 0x2000,
        },
        wm::PAINT => {
            validate(env, hwnd);
            0
        }
        _ => return def_window_proc(env, hwnd, msg, wp, lp, |_, r| Outcome::Return(r)),
    };
    Outcome::Return(r)
}

fn set_button(env: &mut Env<'_>, hwnd: u32, f: impl FnOnce(u32) -> u32) {
    if let Some(w) = env
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
    {
        w.control.button = f(w.control.button);
        w.needs_paint = true;
    }
}

/// Check an auto radio button and uncheck the others in its group.
fn check_radio(env: &mut Env<'_>, hwnd: u32) {
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let Some(parent) = u.windows.get(&env.sys.objects, &hwnd).map(|w| w.parent) else {
        return;
    };
    let siblings = u
        .windows
        .get(&env.sys.objects, &parent)
        .map(|p| p.children.clone())
        .unwrap_or_default();
    let Some(me) = siblings.iter().position(|&s| s == hwnd) else {
        return;
    };
    // The group runs from the previous WS_GROUP control to the next one.
    let group = |h: &u32| {
        u.windows
            .get(&env.sys.objects, h)
            .is_some_and(|w| w.style & WS_GROUP != 0)
    };
    let start = (0..=me).rev().find(|&i| group(&siblings[i])).unwrap_or(0);
    let end = (me + 1..siblings.len())
        .find(|&i| group(&siblings[i]))
        .unwrap_or(siblings.len());
    for &s in &siblings[start..end] {
        if let Some(w) = u.windows.get_mut(&mut env.sys.objects, &s)
            && w.style & 0xf == BS_AUTORADIOBUTTON
        {
            w.control.button = (w.control.button & !3) | u32::from(s == hwnd);
        }
    }
}

fn static_proc(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = args(env);
    match msg {
        // SS_NOTIFY statics take mouse input, others are transparent to it.
        wm::NCHITTEST => {
            let notify = user(env)
                .windows
                .get(&env.sys.objects, &hwnd)
                .is_some_and(|w| w.style & 0x100 != 0);
            Outcome::Return(if notify { 1 } else { u32::MAX })
        }
        wm::PAINT => {
            validate(env, hwnd);
            Outcome::Return(0)
        }
        _ => def_window_proc(env, hwnd, msg, wp, lp, |_, r| Outcome::Return(r)),
    }
}

fn edit_proc(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = args(env);
    match msg {
        wm::GETDLGCODE => Outcome::Return(0x89), // DLGC_WANTCHARS | DLGC_HASSETSEL | DLGC_WANTARROWS
        wm::PAINT => {
            validate(env, hwnd);
            Outcome::Return(0)
        }
        // EM_* (selection, limits, ...) have no visible effect headless.
        0xb0..=0xda => Outcome::Return(0),
        _ => def_window_proc(env, hwnd, msg, wp, lp, |_, r| Outcome::Return(r)),
    }
}

fn trackbar_proc(env: &mut Env<'_>) -> Outcome {
    use super::window::TrackbarState;
    let (hwnd, msg, wp, lp) = args(env);
    if msg == wm::GETDLGCODE {
        return Outcome::Return(0x1);
    } // DLGC_WANTARROWS
    if msg == wm::PAINT {
        validate(env, hwnd);
        return Outcome::Return(0);
    }
    if !(0x400..=0x420).contains(&msg) {
        return def_window_proc(env, hwnd, msg, wp, lp, |_, r| Outcome::Return(r));
    }
    let Some(window) = env
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
    else {
        return Outcome::Return(0);
    };
    let track = window
        .control
        .trackbar
        .get_or_insert_with(TrackbarState::default);
    let result = match msg {
        0x400 => track.value as u32,
        0x401 => track.min as u32,
        0x402 => track.max as u32,
        0x405 => {
            track.value = (lp as i32).clamp(track.min.min(track.max), track.min.max(track.max));
            0
        }
        0x406 => {
            track.min = i32::from(lp as u16 as i16);
            track.max = i32::from((lp >> 16) as u16 as i16);
            track.value = track
                .value
                .clamp(track.min.min(track.max), track.min.max(track.max));
            0
        }
        0x407 | 0x408 => {
            if msg == 0x407 {
                track.min = lp as i32;
            } else {
                track.max = lp as i32;
            }
            track.value = track
                .value
                .clamp(track.min.min(track.max), track.min.max(track.max));
            0
        }
        _ => {
            adv_abi::warn_unavailable(
                env,
                "user32.dll!<TrackbarWndProc>",
                &format!("trackbar message {msg:#x} is unsupported"),
            );
            return Outcome::Return(0);
        }
    };
    if wp != 0 && matches!(msg, 0x405..=0x408) {
        window.needs_paint = true;
    }
    Outcome::Return(result)
}

fn combo_proc(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = args(env);
    if msg == wm::GETDLGCODE {
        return Outcome::Return(0x81);
    }
    if msg == wm::PAINT {
        validate(env, hwnd);
        return Outcome::Return(0);
    }
    if !(0x140..=0x164).contains(&msg) {
        return def_window_proc(env, hwnd, msg, wp, lp, |_, r| Outcome::Return(r));
    }
    let wide = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .is_some_and(|w| w.unicode);
    let source_text = if matches!(msg, 0x143 | 0x14a | 0x14c | 0x14d | 0x158) {
        let Some(text) = super::message::read_text(env, lp, wide) else {
            crate::util::set_last_error(env, 87);
            return Outcome::Return(u32::MAX);
        };
        Some(text)
    } else {
        None
    };
    if matches!(msg, 0x148 | 0x149) {
        let Some(text) = user(env)
            .windows
            .get(&env.sys.objects, &hwnd)
            .and_then(|window| window.control.choices.as_ref())
            .and_then(|choices| choices.items.get(wp as usize))
            .cloned()
        else {
            return Outcome::Return(u32::MAX);
        };
        let length = super::message::text_length(env, &text, wide);
        if msg == 0x148 && super::message::write_text(env, lp, length + 1, &text, wide).is_none() {
            return Outcome::Return(u32::MAX);
        }
        return Outcome::Return(length);
    }
    let Some(window) = env
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
    else {
        return Outcome::Return(u32::MAX);
    };
    let sorted = window.style & 0x100 != 0;
    let choices = window.control.choices.get_or_insert_with(Default::default);
    let result = match msg {
        0x143 | 0x14a => {
            if choices.items.len() >= 16384 {
                return Outcome::Return(0xffff_fffe);
            } // CB_ERRSPACE
            let text = source_text.unwrap();
            let index = if sorted && msg == 0x143 {
                choices
                    .items
                    .partition_point(|item| item.to_lowercase() <= text.to_lowercase())
            } else if msg == 0x14a && (wp as usize) < choices.items.len() {
                wp as usize
            } else {
                choices.items.len()
            };
            choices.items.insert(index, text);
            choices.data.insert(index, 0);
            if let Some(selected) = &mut choices.selected
                && *selected >= index
            {
                *selected += 1;
            }
            index as u32
        }
        0x144 => {
            let index = wp as usize;
            if index >= choices.items.len() {
                return Outcome::Return(u32::MAX);
            }
            choices.items.remove(index);
            choices.data.remove(index);
            if choices.selected == Some(index) {
                choices.selected = None;
                window.text.clear();
            } else if let Some(selected) = &mut choices.selected
                && *selected > index
            {
                *selected -= 1;
            }
            choices.items.len() as u32
        }
        0x146 => choices.items.len() as u32,
        0x147 => choices.selected.map_or(u32::MAX, |i| i as u32),
        0x14b => {
            choices.items.clear();
            choices.data.clear();
            choices.selected = None;
            window.text.clear();
            0
        }
        0x14e => {
            if wp == u32::MAX {
                choices.selected = None;
                window.text.clear();
                u32::MAX
            } else if let Some(text) = choices.items.get(wp as usize) {
                choices.selected = Some(wp as usize);
                window.text = text.clone();
                wp
            } else {
                u32::MAX
            }
        }
        0x14c | 0x14d | 0x158 => {
            let text = source_text.unwrap().to_lowercase();
            let count = choices.items.len();
            let start = if wp == u32::MAX {
                0
            } else {
                (wp as usize).saturating_add(1)
            };
            let found = (0..count)
                .map(|offset| (start + offset) % count)
                .find(|&i| {
                    let item = choices.items[i].to_lowercase();
                    if msg == 0x158 {
                        item == text
                    } else {
                        item.starts_with(&text)
                    }
                });
            if msg == 0x14d
                && let Some(i) = found
            {
                choices.selected = Some(i);
                window.text = choices.items[i].clone();
            }
            found.map_or(u32::MAX, |i| i as u32)
        }
        0x150 => choices.data.get(wp as usize).copied().unwrap_or(u32::MAX),
        0x151 => {
            let Some(data) = choices.data.get_mut(wp as usize) else {
                return Outcome::Return(u32::MAX);
            };
            *data = lp;
            0
        }
        _ => {
            adv_abi::warn_unavailable(
                env,
                "user32.dll!<ComboWndProc>",
                &format!("combo box message {msg:#x} is unsupported"),
            );
            return Outcome::Return(u32::MAX);
        }
    };
    window.needs_paint = true;
    Outcome::Return(result)
}

/// Messages whose dialog-procedure result is returned as-is rather than via DWL_MSGRESULT.
fn returns_directly(msg: u32) -> bool {
    matches!(msg, 0x110 | 0x132..=0x138 | 0x39 | 0x2e | 0x2f | 0x37) // INITDIALOG, CTLCOLOR*, COMPAREITEM, VKEYTOITEM, CHARTOITEM, QUERYDRAGICON
}

fn def_dlg_proc_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = args(env);
    def_dlg_proc(env, hwnd, msg, wp, lp, false)
}

#[win32(user32, name="DefDlgProcW", level=state)]
fn def_dlg_proc_w(env: &mut Env<'_>, hwnd: Handle, msg: u32, wp: u32, lp: i32) -> Outcome {
    def_dlg_proc(env, hwnd.0, msg, wp, lp as u32, true)
}

fn dialog_proc(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = args(env);
    let wide = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .is_some_and(|w| w.unicode);
    def_dlg_proc(env, hwnd, msg, wp, lp, wide)
}

fn def_dlg_proc(env: &mut Env<'_>, hwnd: u32, msg: u32, wp: u32, lp: u32, wide: bool) -> Outcome {
    let dlgproc = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .map_or(0, |w| w.dlgproc());
    if dlgproc == 0 {
        return def_dlg_default(env, hwnd, msg, wp, lp, wide);
    }
    call_proc(dlgproc, hwnd, msg, wp, lp, move |env, r| {
        if r & 0xff != 0 && user(env).windows.contains_key(&env.sys.objects, &hwnd) {
            if returns_directly(msg) {
                return Outcome::Return(r);
            }
            let w = &user(env).windows.get(&env.sys.objects, &hwnd).unwrap();
            let res = w
                .extra
                .get(DWL_MSGRESULT as usize..4)
                .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
            return Outcome::Return(res);
        }
        def_dlg_default(env, hwnd, msg, wp, lp, wide)
    })
}

fn def_dlg_default(
    env: &mut Env<'_>,
    hwnd: u32,
    msg: u32,
    wp: u32,
    lp: u32,
    wide: bool,
) -> Outcome {
    match msg {
        // Closing a dialog is cancelling it.
        wm::CLOSE => {
            let cancel = super::dialog::item(user(env), &env.sys.objects, hwnd, 2);
            post(env, hwnd, wm::COMMAND, 2, cancel);
            Outcome::Return(0)
        }
        wm::INITDIALOG => Outcome::Return(0),
        _ => super::message::def_window_proc_encoded(env, hwnd, msg, wp, lp, wide, |_, r| {
            Outcome::Return(r)
        }),
    }
}

pub fn register(reg: &mut Registry) {
    super::tab::register(reg);
    reg.register(def_dlg_proc_w_ABI);
    stdcall(reg, USER32, DIALOG_PROC, 4, dialog_proc);
    stdcall(reg, USER32, BUTTON_PROC, 4, button_proc);
    stdcall(reg, USER32, STATIC_PROC, 4, static_proc);
    stdcall(reg, USER32, EDIT_PROC, 4, edit_proc);
    stdcall(reg, USER32, COMBO_PROC, 4, combo_proc);
    stdcall(reg, USER32, TRACKBAR_PROC, 4, trackbar_proc);
    stdcall(reg, USER32, "DefDlgProcA", 4, def_dlg_proc_a);
}
