//! MCIWnd owns a USER window; its media device belongs to the common MCI owner.
use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, win32};
use adv_cpu::backend::GuestExt;
use adv_machine::{Conv, Env, Outcome};

use crate::Win32;
use crate::user32::{
    message, user, user_mut,
    window::{self, ClassKey, CreateParams, Rect},
};
use crate::util::set_last_error;

const CLASS: &str = "MCIWndClass";
const PROC: &str = "__pal_mciwnd";
const OPEN_A: u32 = 0x499;
const OPEN_W: u32 = 0x4fc;
const UNSUPPORTED: u32 = 274;

#[derive(Clone, Copy, Default)]
pub(crate) struct State {
    device: u32,
    movie: u32,
    error: u32,
}
fn state(e: &Env<'_>, hwnd: u32) -> Option<State> {
    user(e).windows.get(&e.sys.objects, &hwnd)?.control.mci
}
fn change(e: &mut Env<'_>, hwnd: u32, f: impl FnOnce(&mut State)) {
    if let Some(s) = e
        .ext
        .get_mut::<Win32>()
        .user
        .windows
        .get_mut(&mut e.sys.objects, &hwnd)
        .and_then(|w| w.control.mci.as_mut())
    {
        f(s);
    }
}
fn error(e: &mut Env<'_>, hwnd: u32, code: u32) -> Outcome {
    change(e, hwnd, |s| s.error = code);
    Outcome::Return(code)
}
fn unavailable(e: &mut Env<'_>, hwnd: u32) -> Outcome {
    adv_abi::warn_unavailable(
        e,
        "MCIWnd",
        "window command, styles, or UI controls are unavailable",
    );
    error(e, hwnd, UNSUPPORTED)
}
fn api(
    e: &mut Env<'_>,
    module: &str,
    name: &str,
    args: Vec<u32>,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    match e.sys.thunks.publish(e.cpu, module, name) {
        Ok(p) => Outcome::call(p, args, move |e, r| then(e, r.value())),
        Err(err) => Outcome::Fail(format!("MCIWnd {name}: {err}")),
    }
}
fn mci(
    e: &mut Env<'_>,
    id: u32,
    msg: u32,
    flags: u32,
    words: &[u32],
    wide: bool,
    then: impl FnOnce(&mut Env<'_>, u32, u32) -> Outcome + 'static,
) -> Outcome {
    let p = window::temp_alloc(e, words.len() as u32 * 4);
    if p == 0 {
        return then(e, 264, 0);
    }
    for (i, word) in words.iter().enumerate() {
        if let Err(err) = e.cpu.write_u32(p + i as u32 * 4, *word) {
            window::temp_free(e, p);
            return Outcome::Fail(format!("MCIWnd parameters: {err:?}"));
        }
    }
    api(
        e,
        "winmm.dll",
        if wide {
            "mciSendCommandW"
        } else {
            "mciSendCommandA"
        },
        vec![id, msg, flags, p],
        move |e, code| {
            let out = if code == 0 {
                e.cpu.read_u32(p + 4).unwrap_or(0)
            } else {
                0
            };
            window::temp_free(e, p);
            then(e, code, out)
        },
    )
}
#[win32(msvfw32, name = "MCIWndRegisterClass", level = state)]
fn register_class(e: &mut Env<'_>) -> i32 {
    if user(e)
        .find_class(&ClassKey::Name(CLASS.into()), 0)
        .is_some()
    {
        return 1;
    }
    let Ok(proc) = e.sys.thunks.publish(e.cpu, "msvfw32.dll", PROC) else {
        return 0;
    };
    let u = user_mut(e);
    u.register_class(CLASS, 0, proc, 0, 0, 0, 0, 0);
    // The built-in procedure accepts both CreateWindowEx encodings.
    u.classes.last_mut().unwrap().unicode = None;
    1
}
#[win32(msvfw32, name = "MCIWndCreateA", level = implement)]
fn create_a(
    e: &mut Env<'_>,
    parent: Handle,
    instance: Handle,
    style: u32,
    file: AnsiStr,
) -> Outcome {
    create(e, parent.0, instance.0, style, file.0.addr(), false)
}
#[win32(msvfw32, name = "MCIWndCreateW", level = implement)]
fn create_w(
    e: &mut Env<'_>,
    parent: Handle,
    instance: Handle,
    style: u32,
    file: WideStr,
) -> Outcome {
    create(e, parent.0, instance.0, style, file.0.addr(), true)
}
fn create(
    e: &mut Env<'_>,
    parent: u32,
    instance: u32,
    mut style: u32,
    file: u32,
    wide: bool,
) -> Outcome {
    // A toolbar, recording UI, caption updates and parent notifications require
    // their own observable behavior; do not silently accept those requests.
    if style & 2 == 0 || style & 0xffff & !0xc00f != 0 {
        unavailable(e, 0);
        set_last_error(e, 120);
        return Outcome::Return(0);
    }
    if register_class(e) == 0 {
        return Outcome::Return(0);
    }
    // Both native Create entry points create a Unicode MCIWnd window.
    let file = if file == 0 {
        0
    } else {
        let text = if wide {
            WideStr(GuestPtr::new(file))
                .read(e.cpu, crate::util::MAX_STR)
                .map(|s| String::from_utf16_lossy(&s))
        } else {
            AnsiStr(GuestPtr::new(file))
                .read(e.cpu, crate::util::MAX_STR)
                .map(|s| e.ext.get::<Win32>().codec.decode(&s))
        };
        let Ok(text) = text else {
            set_last_error(e, 998);
            return Outcome::Return(0);
        };
        let pointer = message::guest_text(e, &text, true);
        if pointer == 0 {
            set_last_error(e, 8);
            return Outcome::Return(0);
        }
        pointer
    };
    if style & (window::WS_CHILD | window::WS_POPUP) == 0 {
        style |= if parent == 0 { 0x10cf0000 } else { 0x50800000 };
    }
    style |= 0x06000000; // WS_CLIPCHILDREN | WS_CLIPSIBLINGS
    let params = CreateParams {
        ex_style: 0,
        class: ClassKey::Name(CLASS.into()),
        class_ptr: 0,
        name_ptr: 0,
        text: String::new(),
        style,
        x: 0,
        y: 0,
        w: 320,
        h: 240,
        parent,
        menu: if style & window::WS_CHILD != 0 { 66 } else { 0 },
        instance,
        param: 0,
    };
    window::create_window_encoded(e, params, true, move |e, hwnd| {
        if hwnd == 0 || file == 0 {
            window::temp_free(e, file);
            return Outcome::Return(hwnd);
        }
        message::send(e, hwnd, OPEN_W, 0, file, move |e, _| {
            window::temp_free(e, file);
            Outcome::Return(hwnd)
        })
    })
}
fn close(
    e: &mut Env<'_>,
    hwnd: u32,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let id = state(e, hwnd).map_or(0, |s| s.device);
    if id == 0 {
        return then(e, 0);
    }
    mci(e, id, 0x804, 2, &[0, 0], false, move |e, code, _| {
        if code == 0 {
            change(e, hwnd, |s| {
                s.device = 0;
                s.movie = 0;
            });
        }
        then(e, code)
    })
}
fn open(e: &mut Env<'_>, hwnd: u32, file: u32, wide: bool) -> Outcome {
    close(e, hwnd, move |e, code| {
        if code != 0 {
            return error(e, hwnd, code);
        }
        mci(
            e,
            0,
            0x803,
            0x23202,
            &[0, 0, 520, file, 0, window::WS_CHILD, hwnd],
            wide,
            move |e, code, id| {
                if code != 0 {
                    return error(e, hwnd, code);
                }
                change(e, hwnd, |s| {
                    s.device = id;
                    s.error = 0;
                });
                mci(
                    e,
                    id,
                    0x814,
                    0x102,
                    &[0, 0, 0x4001, 0],
                    false,
                    move |e, code, movie| {
                        if code != 0 {
                            return close(e, hwnd, move |e, _| error(e, hwnd, code));
                        }
                        change(e, hwnd, |s| s.movie = movie);
                        let w = user(e).windows.get(&e.sys.objects, &hwnd).unwrap();
                        if w.style & 1 != 0 {
                            return resize(e, hwnd);
                        }
                        let (l, t, r, b) = window::nc_insets(w.style, w.ex_style);
                        let (width, height) = user(e)
                            .windows
                            .get(&e.sys.objects, &movie)
                            .unwrap()
                            .client_size();
                        window::set_window_pos(
                            e,
                            hwnd,
                            0,
                            Rect::new(0, 0, width + l + r, height + t + b),
                            0x16,
                            move |e, _| resize(e, hwnd),
                        )
                    },
                )
            },
        )
    })
}
fn resize(e: &mut Env<'_>, hwnd: u32) -> Outcome {
    let Some(s) = state(e, hwnd).filter(|s| s.device != 0) else {
        return Outcome::Return(0);
    };
    let w = user(e).windows.get(&e.sys.objects, &hwnd).unwrap();
    if w.style & 4 != 0 {
        return Outcome::Return(0);
    }
    let (width, height) = w.client_size();
    window::set_window_pos(
        e,
        s.movie,
        0,
        Rect::new(0, 0, width, height),
        0x14,
        move |e, _| {
            mci(
                e,
                s.device,
                0x842,
                0x40002,
                &[0, 0, 0, width as u32, height as u32],
                false,
                move |e, code, _| error(e, hwnd, code),
            )
        },
    )
}
fn proc(e: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = (e.arg(0), e.arg(1), e.arg(2), e.arg(3));
    match msg {
        1 => {
            let style = user(e).windows.get(&e.sys.objects, &hwnd).unwrap().style;
            if style & 2 == 0 || style & 0xffff & !0xc00f != 0 {
                unavailable(e, hwnd);
                return Outcome::Return(u32::MAX);
            }
            if let Some(w) = e
                .ext
                .get_mut::<Win32>()
                .user
                .windows
                .get_mut(&mut e.sys.objects, &hwnd)
            {
                w.control.mci = Some(State::default());
            }
            let file = e.cpu.read_u32(lp).unwrap_or(0);
            if file != 0 {
                let wide = user(e).windows.get(&e.sys.objects, &hwnd).unwrap().unicode;
                return message::send(
                    e,
                    hwnd,
                    if wide { OPEN_W } else { OPEN_A },
                    0,
                    file,
                    |_, _| Outcome::Return(0),
                );
            }
            Outcome::Return(0)
        }
        2 => close(e, hwnd, |_, _| Outcome::Return(0)),
        5 => resize(e, hwnd),
        OPEN_A | OPEN_W if wp == 0 => open(e, hwnd, lp, msg == OPEN_W),
        0x804 => close(e, hwnd, move |e, code| error(e, hwnd, code)),
        0x464 => Outcome::Return(state(e, hwnd).map_or(0, |s| s.device)),
        0x488 => Outcome::Return(
            user(e)
                .windows
                .get(&e.sys.objects, &hwnd)
                .map_or(0, |w| w.style & 0xffff),
        ),
        0x480 | 0x4e4 => {
            let code = state(e, hwnd).map_or(0, |s| s.error);
            if wp == 0 {
                return Outcome::Return(code);
            }
            api(
                e,
                "winmm.dll",
                if msg == 0x480 {
                    "mciGetErrorStringA"
                } else {
                    "mciGetErrorStringW"
                },
                vec![code, lp, wp],
                move |_, _| Outcome::Return(code),
            )
        }
        0x490 | 0x491 => Outcome::Return(u32::from(state(e, hwnd).is_some_and(|s| s.device != 0))),
        0x492..=0x495 => Outcome::Return(0),
        0x806 | 0x808 | 0x809 | 0x855 => {
            let id = state(e, hwnd).map_or(0, |s| s.device);
            if id == 0 {
                return error(e, hwnd, 257);
            }
            mci(e, id, msg, 0, &[0, 0, 0], false, move |e, code, _| {
                error(e, hwnd, code)
            })
        }
        0x466 | 0x4ca | 0x468 | 0x469 | 0x46a | 0x4ce | 0x46f => {
            if wp != 0 {
                return unavailable(e, hwnd);
            }
            let id = state(e, hwnd).map_or(0, |s| s.device);
            if id == 0 {
                return Outcome::Return(if matches!(msg, 0x46a | 0x4ce) { 524 } else { 0 });
            }
            let item = match msg {
                0x466 | 0x4ca => 2,
                0x468 | 0x469 => 1,
                0x46f => 0x4019,
                _ => 4,
            };
            mci(
                e,
                id,
                0x814,
                0x102,
                &[0, 0, item, 0],
                false,
                move |e, code, value| {
                    change(e, hwnd, |s| s.error = code);
                    Outcome::Return(if code == 0 { value } else { 0 })
                },
            )
        }
        0x46e => {
            let id = state(e, hwnd).map_or(0, |s| s.device);
            if id == 0 {
                return error(e, hwnd, 257);
            }
            mci(
                e,
                id,
                0x873,
                0x1800002,
                &[0, 0x4002, lp, 0, 0, 0],
                false,
                move |e, code, _| error(e, hwnd, code),
            )
        }
        0x467 => Outcome::Return(0),
        0x400..=0x8ff => unavailable(e, hwnd),
        _ => message::def_window_proc(e, hwnd, msg, wp, lp, |_, r| Outcome::Return(r)),
    }
}
pub(super) fn register(r: &mut Registry) {
    r.register(create_a_ABI);
    r.register(create_w_ABI);
    r.register(register_class_ABI);
    r.add("msvfw32.dll", PROC, Conv::Stdcall { argc: 4 }, proc);
}
