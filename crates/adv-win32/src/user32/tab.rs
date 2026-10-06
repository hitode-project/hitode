//! Native tab messages remain reachable through guest subclasses and saved procs.
use super::{
    USER32,
    message::{def_window_proc, validate},
    user,
    window::wm,
};
use crate::{Win32, stdcall};
use adv_abi::{AnsiStr, Env, GuestPtr, Outcome, Registry, WideStr};
use adv_cpu::backend::Prot;

pub(super) const PROC: &str = "<TabWndProc>";
#[derive(Clone)]
struct Item {
    text: Vec<u16>,
    image: i32,
    parameter: u32,
}
impl Default for Item {
    fn default() -> Self {
        Self {
            text: Vec::new(),
            image: -1,
            parameter: 0,
        }
    }
}
pub(crate) struct TabState {
    items: Vec<Item>,
    selected: i32,
}
impl Default for TabState {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            selected: -1,
        }
    }
}
fn state<'a>(env: &'a mut Env<'_>, hwnd: u32) -> &'a mut TabState {
    env.ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
        .unwrap()
        .control
        .tab
        .get_or_insert_with(TabState::default)
}
fn item(
    env: &mut Env<'_>,
    pointer: u32,
    wide: bool,
    insert: bool,
) -> Option<([u32; 7], Option<Vec<u16>>)> {
    let values = GuestPtr::<[u32; 7]>::new(pointer).read(env.cpu).ok()?;
    if values[0] & !0x1f != 0 || !insert && values[0] & 0x10 != 0 {
        adv_abi::warn_unavailable(
            env,
            "SysTabControl32",
            "custom item attributes and state setters are not supported",
        );
        return None;
    }
    let text = if values[0] & 1 == 0 {
        None
    } else if values[3] == 0 {
        Some(Vec::new())
    } else if values[3] == u32::MAX {
        adv_abi::warn_unavailable(
            env,
            "SysTabControl32",
            "callback item text is not supported",
        );
        return None;
    } else if wide {
        Some(
            WideStr(GuestPtr::new(values[3]))
                .read(env.cpu, 512 * 1024)
                .ok()?,
        )
    } else {
        let raw = AnsiStr(GuestPtr::new(values[3]))
            .read(env.cpu, 1024 * 1024)
            .ok()?;
        Some(env.ext.get::<Win32>().codec.to_utf16(0, &raw))
    };
    Some((values, text))
}
fn update(item: &mut Item, values: [u32; 7], text: Option<Vec<u16>>) {
    if let Some(text) = text {
        item.text = text;
    }
    if values[0] & 2 != 0 {
        item.image = values[5] as i32;
    }
    if values[0] & 8 != 0 {
        item.parameter = values[6];
    }
}
fn get(env: &mut Env<'_>, hwnd: u32, index: u32, pointer: u32, wide: bool) -> Outcome {
    let Some(item) = state(env, hwnd).items.get(index as usize).cloned() else {
        return Outcome::Return(0);
    };
    let Ok(values) = GuestPtr::<[u32; 7]>::new(pointer).read(env.cpu) else {
        return Outcome::Return(0);
    };
    if values[0] & !0x1f != 0 {
        return Outcome::Return(0);
    }
    let mut writes = Vec::new();
    if values[0] & 0x10 != 0 {
        writes.push((
            pointer + 4,
            ((u32::from(state(env, hwnd).selected == index as i32)) & values[2])
                .to_le_bytes()
                .to_vec(),
        ));
    }
    if values[0] & 2 != 0 {
        writes.push((pointer + 20, item.image.to_le_bytes().to_vec()));
    }
    if values[0] & 8 != 0 {
        writes.push((pointer + 24, item.parameter.to_le_bytes().to_vec()));
    }
    if values[0] & 1 != 0 && values[4] as i32 > 0 {
        let capacity = values[4] as usize;
        let text = if wide {
            item.text
                .iter()
                .copied()
                .take(capacity - 1)
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect()
        } else {
            let mut raw = env
                .ext
                .get::<Win32>()
                .codec
                .encode(&String::from_utf16_lossy(&item.text));
            raw.truncate(capacity - 1);
            raw.push(0);
            raw
        };
        writes.push((values[3], text));
    }
    if writes.iter().any(|(p, bytes)| {
        adv_abi::guest::check_range(env.cpu, *p, bytes.len() as u32, Prot::WRITE).is_err()
    }) {
        return Outcome::Return(0);
    }
    for (p, bytes) in writes {
        if env.cpu.write(p, &bytes).is_err() {
            return Outcome::Return(0);
        }
    }
    Outcome::Return(1)
}
fn proc(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, index, pointer) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if !user(env).windows.contains_key(&env.sys.objects, &hwnd) {
        return Outcome::Return(0);
    }
    let result = match msg {
        0x1304 => state(env, hwnd).items.len() as u32,
        0x130b => state(env, hwnd).selected as u32,
        0x130c => {
            let tabs = state(env, hwnd);
            if index != u32::MAX && index as usize >= tabs.items.len() {
                u32::MAX
            } else {
                std::mem::replace(&mut tabs.selected, index as i32) as u32
            }
        }
        0x1305 | 0x133c => return get(env, hwnd, index, pointer, msg == 0x133c),
        0x1306 | 0x133d => {
            if index as usize >= state(env, hwnd).items.len() {
                return Outcome::Return(0);
            }
            let Some((values, text)) = item(env, pointer, msg == 0x133d, false) else {
                return Outcome::Return(0);
            };
            update(&mut state(env, hwnd).items[index as usize], values, text);
            1
        }
        0x1307 | 0x133e => {
            if (index as i32) < 0 {
                return Outcome::Return(u32::MAX);
            }
            let Some((values, text)) = item(env, pointer, msg == 0x133e, true) else {
                return Outcome::Return(u32::MAX);
            };
            let tabs = state(env, hwnd);
            let index = (index as usize).min(tabs.items.len());
            let mut entry = Item::default();
            update(&mut entry, values, text);
            let first = tabs.items.is_empty();
            tabs.items.insert(index, entry);
            if first {
                tabs.selected = 0;
            } else if tabs.selected >= index as i32 {
                tabs.selected += 1;
            }
            index as u32
        }
        0x1308 => {
            let tabs = state(env, hwnd);
            if index as usize >= tabs.items.len() {
                0
            } else {
                tabs.items.remove(index as usize);
                if tabs.selected == index as i32 {
                    tabs.selected = -1;
                } else if tabs.selected > index as i32 {
                    tabs.selected -= 1;
                }
                1
            }
        }
        0x1309 => {
            let tabs = state(env, hwnd);
            tabs.items.clear();
            tabs.selected = -1;
            1
        }
        wm::PAINT => {
            validate(env, hwnd);
            0
        }
        0x1300..=0x1340 => {
            return Outcome::Fail(format!(
                "SysTabControl32: message {msg:#x} is not implemented"
            ));
        }
        _ => {
            return def_window_proc(env, hwnd, msg, index, pointer, |_, result| {
                Outcome::Return(result)
            });
        }
    };
    Outcome::Return(result)
}
pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, USER32, PROC, 4, proc);
}

#[cfg(test)]
mod tests;
