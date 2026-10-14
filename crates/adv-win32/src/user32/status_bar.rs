//! msctls_statusbar32 and CreateStatusWindowA/W: text parts along the bottom
//! of the parent's client area. The guest sees parts, text and geometry; the
//! host projects the text, so owner-drawn parts, icons and tips are unavailable.
use super::{
    USER32,
    message::{def_window_proc, read_text, text_length, validate, write_text},
    user,
    window::{
        ClassKey, CreateParams, Rect, SWP_NOACTIVATE, SWP_NOZORDER, create_window_encoded,
        set_window_pos, wm,
    },
};
use crate::{Win32, stdcall};
use adv_abi::{Env, Outcome, Registry};
use adv_cpu::backend::Prot;
use std::collections::BTreeMap;

pub(super) const PROC: &str = "<StatusBarWndProc>";
pub const CLASS: &str = "msctls_statusbar32";
const SIMPLE: u8 = 255;
const CCS_TOP: u32 = 0x1;
const CCS_NORESIZE: u32 = 0x4;
const CCS_NOPARENTALIGN: u32 = 0x8;
/// Classic 96-DPI height with the default GUI font, and the borders
/// SB_GETBORDERS reports: horizontal, vertical, between parts.
const HEIGHT: i32 = 22;
const BORDERS: [i32; 3] = [0, 2, 2];
const SBT_OWNERDRAW: u32 = 0x1000;

pub(crate) struct StatusState {
    /// Right edges of the parts; -1 extends to the right edge.
    edges: Vec<i32>,
    /// Text and SBT_* type by part. Part 0's text is the window text, as WM_SETTEXT sets it.
    texts: BTreeMap<u8, (String, u32)>,
    simple: bool,
    min_height: i32,
}
impl Default for StatusState {
    fn default() -> Self {
        Self {
            edges: vec![-1],
            texts: BTreeMap::new(),
            simple: false,
            min_height: 0,
        }
    }
}
impl StatusState {
    fn height(&self) -> i32 {
        HEIGHT.max(self.min_height + 2 * BORDERS[1])
    }
    /// The text a host shows: the simple text, or the parts in order.
    pub(crate) fn display(&self, part0: &str) -> String {
        if self.simple {
            return self
                .texts
                .get(&SIMPLE)
                .map_or_else(String::new, |(t, _)| t.clone());
        }
        (0..self.edges.len() as u8)
            .map(|part| match part {
                0 => part0,
                _ => self.texts.get(&part).map_or("", |(t, _)| t.as_str()),
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("    ")
    }
}

/// Where a status bar sits in a parent client area of `parent` size.
fn placement(style: u32, current: Rect, parent: (i32, i32), height: i32) -> Option<Rect> {
    if style & CCS_NORESIZE != 0 {
        return None;
    }
    let top = if style & CCS_NOPARENTALIGN != 0 {
        current.top
    } else if style & 3 == CCS_TOP {
        0
    } else {
        parent.1 - height
    };
    Some(Rect::new(0, top, parent.0, height))
}

fn layout(env: &Env<'_>, hwnd: u32) -> Option<Rect> {
    let user = user(env);
    let window = user.windows.get(&env.sys.objects, &hwnd)?;
    let parent = user.windows.get(&env.sys.objects, &window.parent)?;
    let height = window
        .control
        .status
        .as_ref()
        .map_or(HEIGHT, StatusState::height);
    placement(window.style, window.rect, parent.client_size(), height)
        .filter(|rect| *rect != window.rect)
}

fn state<'a>(env: &'a mut Env<'_>, hwnd: u32) -> Option<&'a mut StatusState> {
    Some(
        env.ext
            .get::<Win32>()
            .user
            .windows
            .get_mut(&mut env.sys.objects, &hwnd)?
            .control
            .status
            .get_or_insert_with(StatusState::default),
    )
}

fn part_text(env: &mut Env<'_>, hwnd: u32, part: u8) -> (String, u32) {
    let text = if part == 0 {
        user(env)
            .windows
            .get(&env.sys.objects, &hwnd)
            .map(|w| w.text.clone())
    } else {
        None
    };
    let entry = state(env, hwnd).and_then(|s| s.texts.get(&part).cloned());
    match (text, entry) {
        (Some(text), entry) => (text, entry.map_or(0, |(_, kind)| kind)),
        (None, entry) => entry.unwrap_or_default(),
    }
}

fn proc(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let result = match msg {
        wm::PAINT => {
            validate(env, hwnd);
            0
        }
        // The parent forwards WM_SIZE; the bar takes its own place again.
        wm::SIZE => {
            return match layout(env, hwnd) {
                Some(rect) => {
                    set_window_pos(env, hwnd, 0, rect, SWP_NOZORDER | SWP_NOACTIVATE, |_, _| {
                        Outcome::Return(0)
                    })
                }
                None => Outcome::Return(0),
            };
        }
        0x401 | 0x40b => {
            let (part, kind) = ((wp & 0xff) as u8, wp & 0xff00);
            let text = if kind & SBT_OWNERDRAW != 0 {
                adv_abi::warn_unavailable(
                    env,
                    CLASS,
                    "owner-drawn parts are not painted; their data is kept",
                );
                String::new()
            } else {
                let Some(text) = read_text(env, lp, msg == 0x40b) else {
                    return Outcome::Return(0);
                };
                text
            };
            if part == 0 {
                if let Some(window) = env
                    .ext
                    .get::<Win32>()
                    .user
                    .windows
                    .get_mut(&mut env.sys.objects, &hwnd)
                {
                    window.text = text.clone();
                }
            }
            if let Some(status) = state(env, hwnd) {
                status.texts.insert(part, (text, kind));
            }
            if let Some(window) = env
                .ext
                .get::<Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &hwnd)
            {
                window.invalidate_client(true);
            }
            1
        }
        // SB_GETTEXT and SB_GETTEXTLENGTH: MAKELONG(length, type).
        0x402 | 0x40d | 0x403 | 0x40c => {
            let wide = matches!(msg, 0x40d | 0x40c);
            let (text, kind) = part_text(env, hwnd, (wp & 0xff) as u8);
            let length = text_length(env, &text, wide);
            if matches!(msg, 0x402 | 0x40d) && lp != 0 {
                let capacity = length + 1;
                if write_text(env, lp, capacity, &text, wide).is_none() {
                    return Outcome::Return(0);
                }
            }
            (length & 0xffff) | (kind << 16)
        }
        0x404 => {
            if wp == 0 || wp > 256 {
                return Outcome::Return(0);
            }
            let mut bytes = vec![0; wp as usize * 4];
            if adv_abi::guest::check_range(env.cpu, lp, wp * 4, Prot::READ).is_err()
                || env.cpu.read(lp, &mut bytes).is_err()
            {
                return Outcome::Return(0);
            }
            let edges = bytes
                .chunks_exact(4)
                .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
                .collect();
            if let Some(status) = state(env, hwnd) {
                status.edges = edges;
            }
            1
        }
        0x406 => {
            let edges = state(env, hwnd).map_or_else(Vec::new, |s| s.edges.clone());
            if lp != 0 {
                let count = edges.len().min(wp as usize);
                let bytes: Vec<u8> = edges[..count]
                    .iter()
                    .flat_map(|e| e.to_le_bytes())
                    .collect();
                if adv_abi::guest::check_range(env.cpu, lp, bytes.len() as u32, Prot::WRITE)
                    .is_err()
                    || env.cpu.write(lp, &bytes).is_err()
                {
                    return Outcome::Return(0);
                }
            }
            edges.len() as u32
        }
        0x407 => {
            let bytes: Vec<u8> = BORDERS.iter().flat_map(|b| b.to_le_bytes()).collect();
            u32::from(
                adv_abi::guest::check_range(env.cpu, lp, 12, Prot::WRITE).is_ok()
                    && env.cpu.write(lp, &bytes).is_ok(),
            )
        }
        0x408 => {
            if let Some(status) = state(env, hwnd) {
                status.min_height = wp as i32;
            }
            0
        }
        0x409 => {
            if let Some(status) = state(env, hwnd) {
                status.simple = wp != 0;
            }
            if let Some(window) = env
                .ext
                .get::<Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &hwnd)
            {
                window.invalidate_client(true);
            }
            0
        }
        0x40a => return part_rect(env, hwnd, wp, lp),
        0x40e => u32::from(state(env, hwnd).is_some_and(|s| s.simple)),
        // Icons, tips and colours are painted by the host's own control.
        0x40f | 0x410 | 0x411 | 0x2001 => {
            adv_abi::warn_unavailable(
                env,
                CLASS,
                &format!("status bar message {msg:#x} has no host appearance"),
            );
            0
        }
        0x400..=0x414 => {
            return Outcome::Fail(format!("{CLASS}: message {msg:#x} is not implemented"));
        }
        _ => return def_window_proc(env, hwnd, msg, wp, lp, |_, r| Outcome::Return(r)),
    };
    Outcome::Return(result)
}

/// SB_GETRECT: the part's rectangle in the bar's client area.
fn part_rect(env: &mut Env<'_>, hwnd: u32, part: u32, pointer: u32) -> Outcome {
    let Some(window) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        return Outcome::Return(0);
    };
    let (width, height) = window.client_size();
    let Some(status) = state(env, hwnd) else {
        return Outcome::Return(0);
    };
    let edges = if status.simple {
        vec![-1]
    } else {
        status.edges.clone()
    };
    let Some(&edge) = edges.get(part as usize) else {
        return Outcome::Return(0);
    };
    let left = if part == 0 {
        BORDERS[0]
    } else {
        edges[part as usize - 1] + BORDERS[2]
    };
    let right = if edge < 0 { width } else { edge };
    let rect = [left, BORDERS[1], right, height - BORDERS[1]];
    let bytes: Vec<u8> = rect.iter().flat_map(|v| v.to_le_bytes()).collect();
    Outcome::Return(u32::from(
        adv_abi::guest::check_range(env.cpu, pointer, 16, Prot::WRITE).is_ok()
            && env.cpu.write(pointer, &bytes).is_ok(),
    ))
}

/// CreateStatusWindow(style, text, parent, id): a status bar already placed
/// along the parent's bottom edge (or top edge with CCS_TOP).
fn create_status_window(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (style, text_ptr, parent, id) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let Some(text) = read_text(env, text_ptr, wide) else {
        crate::util::set_last_error(env, 998);
        return Outcome::Return(0);
    };
    let user = user(env);
    let Some(class) = user.find_class(&ClassKey::Name(CLASS.into()), 0) else {
        return Outcome::Fail(format!("{CLASS} is not registered"));
    };
    let atom = user.classes[class].atom;
    let parent_size = user
        .windows
        .get(&env.sys.objects, &parent)
        .map_or((0, 0), |w| w.client_size());
    let rect = placement(style, Rect::new(0, 0, 0, 0), parent_size, HEIGHT)
        .unwrap_or(Rect::new(0, 0, 0, 0));
    let params = CreateParams {
        ex_style: 0,
        class: ClassKey::Atom(atom),
        class_ptr: u32::from(atom),
        name_ptr: text_ptr,
        text,
        style,
        x: rect.left as u32,
        y: rect.top as u32,
        w: rect.width() as u32,
        h: rect.height() as u32,
        parent,
        menu: id,
        instance: 0,
        param: 0,
    };
    create_window_encoded(env, params, wide, |_, hwnd| Outcome::Return(hwnd))
}

pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, USER32, PROC, 4, proc);
    stdcall(registry, "comctl32.dll", "CreateStatusWindowA", 4, |env| {
        create_status_window(env, false)
    });
    stdcall(registry, "comctl32.dll", "CreateStatusWindowW", 4, |env| {
        create_status_window(env, true)
    });
}

#[cfg(test)]
mod tests;
