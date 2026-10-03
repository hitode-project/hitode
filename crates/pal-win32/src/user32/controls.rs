//! Built-in window classes (BUTTON, STATIC, EDIT) and the dialog procedure DefDlgProc.
//! They are host functions reached through thunks, like any window procedure, so guest
//! subclassing (SetWindowLong(GWL_WNDPROC) + CallWindowProc) works on them.

use pal_machine::{Env, Outcome, Registry};

use super::message::{call_proc, def_window_proc, post, send, validate};
use super::window::{DWL_MSGRESULT, make_long, wm};
use super::{USER32, user, user_mut};
use crate::stdcall;

pub const BUTTON_PROC: &str = "<ButtonWndProc>";
pub const STATIC_PROC: &str = "<StaticWndProc>";
pub const EDIT_PROC: &str = "<EditWndProc>";

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
    let Some(w) = user(env).windows.get(&hwnd) else { return Outcome::Return(0) };
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
            set_button(env, hwnd, |s| if wp != 0 { s | BST_PUSHED } else { s & !BST_PUSHED });
            0
        }
        BM_SETSTYLE => {
            if let Some(w) = user_mut(env).windows.get_mut(&hwnd) {
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
            let ctx = env.sys.current;
            user_mut(env).queue(ctx).capture = hwnd;
            0
        }
        wm::LBUTTONUP => {
            let pushed = w.control.button & BST_PUSHED != 0;
            set_button(env, hwnd, |s| s & !BST_PUSHED);
            let ctx = env.sys.current;
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
            return send(env, parent, wm::COMMAND, make_long(id as i32, BN_CLICKED as i32), hwnd, |_, _| {
                Outcome::Return(0)
            });
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
    if let Some(w) = user_mut(env).windows.get_mut(&hwnd) {
        w.control.button = f(w.control.button);
        w.needs_paint = true;
    }
}

/// Check an auto radio button and uncheck the others in its group.
fn check_radio(env: &mut Env<'_>, hwnd: u32) {
    let u = user_mut(env);
    let Some(parent) = u.windows.get(&hwnd).map(|w| w.parent) else { return };
    let siblings = u.windows.get(&parent).map(|p| p.children.clone()).unwrap_or_default();
    let Some(me) = siblings.iter().position(|&s| s == hwnd) else { return };
    // The group runs from the previous WS_GROUP control to the next one.
    let group = |h: &u32| u.windows.get(h).is_some_and(|w| w.style & WS_GROUP != 0);
    let start = (0..=me).rev().find(|&i| group(&siblings[i])).unwrap_or(0);
    let end = (me + 1..siblings.len()).find(|&i| group(&siblings[i])).unwrap_or(siblings.len());
    for &s in &siblings[start..end] {
        if let Some(w) = u.windows.get_mut(&s)
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
            let notify = user(env).windows.get(&hwnd).is_some_and(|w| w.style & 0x100 != 0);
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

/// Messages whose dialog-procedure result is returned as-is rather than via DWL_MSGRESULT.
fn returns_directly(msg: u32) -> bool {
    matches!(msg, 0x110 | 0x132..=0x138 | 0x39 | 0x2e | 0x2f | 0x37) // INITDIALOG, CTLCOLOR*, COMPAREITEM, VKEYTOITEM, CHARTOITEM, QUERYDRAGICON
}

fn def_dlg_proc_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = args(env);
    let dlgproc = user(env).windows.get(&hwnd).map_or(0, |w| w.dlgproc());
    if dlgproc == 0 {
        return def_dlg_default(env, hwnd, msg, wp, lp);
    }
    call_proc(dlgproc, hwnd, msg, wp, lp, move |env, r| {
        if r & 0xff != 0 && user(env).windows.contains_key(&hwnd) {
            if returns_directly(msg) {
                return Outcome::Return(r);
            }
            let w = &user(env).windows[&hwnd];
            let res = w.extra.get(DWL_MSGRESULT as usize..4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
            return Outcome::Return(res);
        }
        def_dlg_default(env, hwnd, msg, wp, lp)
    })
}

fn def_dlg_default(env: &mut Env<'_>, hwnd: u32, msg: u32, wp: u32, lp: u32) -> Outcome {
    match msg {
        // Closing a dialog is cancelling it.
        wm::CLOSE => {
            let cancel = super::dialog::item(user(env), hwnd, 2);
            post(env, hwnd, wm::COMMAND, 2, cancel);
            Outcome::Return(0)
        }
        wm::INITDIALOG => Outcome::Return(0),
        _ => def_window_proc(env, hwnd, msg, wp, lp, |_, r| Outcome::Return(r)),
    }
}

pub fn register(reg: &mut Registry) {
    stdcall(reg, USER32, BUTTON_PROC, 4, button_proc);
    stdcall(reg, USER32, STATIC_PROC, 4, static_proc);
    stdcall(reg, USER32, EDIT_PROC, 4, edit_proc);
    stdcall(reg, USER32, "DefDlgProcA", 4, def_dlg_proc_a);
}
