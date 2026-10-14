//! The ListBox class: single-selection string lists. The guest sees items,
//! item data, selection and the top index; the host projects the strings.
//! Multiple selection and owner-drawn items without strings are unavailable.
use super::{
    USER32,
    message::{def_window_proc, read_text, text_length, validate, write_text},
    user,
    window::{ChoiceState, wm},
};
use crate::{Win32, stdcall};
use adv_abi::{Env, Outcome, Registry};

pub(super) const PROC: &str = "<ListBoxWndProc>";
const LB_ERR: u32 = u32::MAX;
const LB_ERRSPACE: u32 = 0xffff_fffe;
const LBS_SORT: u32 = 0x2;
const LBS_MULTIPLESEL: u32 = 0x8;
const LBS_OWNERDRAWFIXED: u32 = 0x10;
const LBS_OWNERDRAWVARIABLE: u32 = 0x20;
const LBS_HASSTRINGS: u32 = 0x40;
const LBS_EXTENDEDSEL: u32 = 0x800;

/// Whether a host can show this list: one selection and string items.
pub(crate) fn projectable(style: u32) -> bool {
    let owner_draw = style & (LBS_OWNERDRAWFIXED | LBS_OWNERDRAWVARIABLE) != 0;
    style & (LBS_MULTIPLESEL | LBS_EXTENDEDSEL) == 0 && (!owner_draw || style & LBS_HASSTRINGS != 0)
}

fn find(choices: &ChoiceState, start: u32, text: &str, exact: bool) -> u32 {
    let count = choices.items.len();
    let text = text.to_lowercase();
    let start = if start == LB_ERR || start as usize >= count {
        0
    } else {
        start as usize + 1
    };
    (0..count)
        .map(|offset| (start + offset) % count)
        .find(|&i| {
            let item = choices.items[i].to_lowercase();
            if exact {
                item == text
            } else {
                item.starts_with(&text)
            }
        })
        .map_or(LB_ERR, |i| i as u32)
}

fn proc(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if msg == wm::GETDLGCODE {
        return Outcome::Return(0x81); // DLGC_WANTARROWS | DLGC_WANTCHARS
    }
    if msg == wm::PAINT {
        validate(env, hwnd);
        return Outcome::Return(0);
    }
    if !(0x180..=0x1b0).contains(&msg) {
        return def_window_proc(env, hwnd, msg, wp, lp, |_, r| Outcome::Return(r));
    }
    let Some(window) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        return Outcome::Return(LB_ERR);
    };
    let (wide, style) = (window.unicode, window.style);
    if style & (LBS_MULTIPLESEL | LBS_EXTENDEDSEL) != 0 && matches!(msg, 0x185 | 0x186 | 0x188) {
        adv_abi::warn_unavailable(
            env,
            "user32.dll!<ListBoxWndProc>",
            "multiple-selection list boxes are unavailable",
        );
        return Outcome::Return(LB_ERR);
    }
    let strings = projectable(style & !(LBS_MULTIPLESEL | LBS_EXTENDEDSEL));
    let source_text = if strings && matches!(msg, 0x180 | 0x181 | 0x18c | 0x18f | 0x1a2) {
        let Some(text) = read_text(env, lp, wide) else {
            crate::util::set_last_error(env, 87);
            return Outcome::Return(LB_ERR);
        };
        Some(text)
    } else {
        None
    };
    if matches!(msg, 0x189 | 0x18a) {
        let Some(text) = user(env)
            .windows
            .get(&env.sys.objects, &hwnd)
            .and_then(|window| window.control.choices.as_ref())
            .and_then(|choices| choices.items.get(wp as usize))
            .cloned()
        else {
            return Outcome::Return(LB_ERR);
        };
        let length = text_length(env, &text, wide);
        if msg == 0x189 && write_text(env, lp, length + 1, &text, wide).is_none() {
            return Outcome::Return(LB_ERR);
        }
        return Outcome::Return(length);
    }
    let Some(window) = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
    else {
        return Outcome::Return(LB_ERR);
    };
    let choices = window.control.choices.get_or_insert_with(Default::default);
    let count = choices.items.len();
    let result = match msg {
        // LB_ADDSTRING, LB_INSERTSTRING
        0x180 | 0x181 => {
            if count >= 32767 {
                return Outcome::Return(LB_ERRSPACE);
            }
            if msg == 0x181 && wp != LB_ERR && wp as usize > count {
                return Outcome::Return(LB_ERR);
            }
            let text = source_text.unwrap_or_default();
            let index = if msg == 0x180 && style & LBS_SORT != 0 && strings {
                let key = text.to_lowercase();
                choices
                    .items
                    .partition_point(|item| item.to_lowercase() <= key)
            } else if msg == 0x181 && wp != LB_ERR {
                wp as usize
            } else {
                count
            };
            choices.items.insert(index, text);
            choices.data.insert(index, if strings { 0 } else { lp });
            if let Some(selected) = &mut choices.selected
                && *selected >= index
            {
                *selected += 1;
            }
            if choices.top > index {
                choices.top += 1;
            }
            index as u32
        }
        // LB_DELETESTRING
        0x182 => {
            let index = wp as usize;
            if index >= count {
                return Outcome::Return(LB_ERR);
            }
            choices.items.remove(index);
            choices.data.remove(index);
            match choices.selected {
                Some(selected) if selected == index => choices.selected = None,
                Some(selected) if selected > index => choices.selected = Some(selected - 1),
                _ => {}
            }
            choices.top = choices.top.min(choices.items.len().saturating_sub(1));
            choices.items.len() as u32
        }
        // LB_RESETCONTENT
        0x184 => {
            *choices = ChoiceState::default();
            0
        }
        // LB_SETCURSEL: -1 clears the selection and still reports LB_ERR.
        0x186 => {
            if (wp as usize) < count {
                choices.selected = Some(wp as usize);
                wp
            } else {
                if wp == LB_ERR {
                    choices.selected = None;
                }
                LB_ERR
            }
        }
        // LB_GETSEL
        0x187 => {
            if wp as usize >= count {
                LB_ERR
            } else {
                u32::from(choices.selected == Some(wp as usize))
            }
        }
        0x188 => choices.selected.map_or(LB_ERR, |i| i as u32),
        0x18b => count as u32,
        // LB_SELECTSTRING
        0x18c => {
            let found = find(choices, wp, &source_text.unwrap_or_default(), false);
            if found != LB_ERR {
                choices.selected = Some(found as usize);
            }
            found
        }
        0x18e => choices.top as u32,
        // LB_FINDSTRING, LB_FINDSTRINGEXACT
        0x18f | 0x1a2 => find(choices, wp, &source_text.unwrap_or_default(), msg == 0x1a2),
        // LB_SETTOPINDEX
        0x197 => {
            if wp as usize >= count.max(1) {
                return Outcome::Return(LB_ERR);
            }
            choices.top = wp as usize;
            0
        }
        0x199 => choices.data.get(wp as usize).copied().unwrap_or(LB_ERR),
        // LB_SETITEMDATA
        0x19a => {
            let Some(data) = choices.data.get_mut(wp as usize) else {
                return Outcome::Return(LB_ERR);
            };
            *data = lp;
            0
        }
        // LB_INITSTORAGE reports the items storage can hold.
        0x1a8 => (count as u32).saturating_add(wp),
        _ => {
            adv_abi::warn_unavailable(
                env,
                "user32.dll!<ListBoxWndProc>",
                &format!("list box message {msg:#x} is unsupported"),
            );
            return Outcome::Return(LB_ERR);
        }
    };
    window.invalidate_client(true);
    Outcome::Return(result)
}

pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, USER32, PROC, 4, proc);
}

#[cfg(test)]
mod tests;
