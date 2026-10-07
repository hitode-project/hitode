//! Radio checks edit the existing menu payload, not an independent check cache.
use super::{get, get_mut, user};
use crate::util::set_last_error;
use adv_abi::{Env, Handle, Registry, win32};
use std::collections::BTreeSet;

const API: &str = "user32.dll!CheckMenuRadioItem";
const BY_POSITION: u32 = 0x400;
const CHECKED: u32 = 8;
const RADIO: u32 = 0x200;

fn fail(env: &mut Env<'_>, error: u32) -> i32 {
    set_last_error(env, error);
    0
}

fn command_menu(env: &Env<'_>, root: u32, first: u32, last: u32) -> Result<Option<u32>, u32> {
    let menus = &user(env).menus;
    if let Some((menu, _)) = menus.locate(&env.sys.objects, root, first, false) {
        return Ok(Some(menu));
    }
    // Native skips missing IDs before finding the first member of the group.
    // Find the lowest existing ID instead of iterating an arbitrary UINT span.
    let mut pending = vec![(root, 0)];
    let mut visited = BTreeSet::new();
    let mut remaining = 16384;
    let mut lowest = None;
    while let Some((menu, begin)) = pending.pop() {
        if begin == 0 && !visited.insert(menu) {
            continue;
        }
        let Some(items) = menus.get(&env.sys.objects, menu) else {
            return Err(1401);
        };
        for (at, item) in items.iter().enumerate().skip(begin) {
            if remaining == 0 {
                return Err(8);
            }
            remaining -= 1;
            if item.id >= first && item.id <= last && lowest.is_none_or(|(id, _)| item.id < id) {
                lowest = Some((item.id, menu));
            }
            if item.submenu != 0 {
                pending.push((menu, at + 1));
                pending.push((item.submenu, 0));
                break;
            }
        }
    }
    Ok(lowest.map(|(_, menu)| menu))
}

#[win32(user32, name = "CheckMenuRadioItem", level = state)]
fn radio(env: &mut Env<'_>, root: Handle, first: u32, last: u32, checked: u32, flags: u32) -> i32 {
    if get(env, root.0).is_none() {
        return fail(env, 1401);
    }
    if flags & !BY_POSITION != 0 {
        // Some native versions ignore reserved flag bits. This front advertises
        // only the two documented addressing modes rather than guessing extras.
        adv_abi::warn_unavailable(
            env,
            API,
            "radio flags outside MF_BYCOMMAND/MF_BYPOSITION are unsupported",
        );
        return fail(env, 50);
    }
    if first > last {
        return fail(env, 1456);
    }
    let by_position = flags & BY_POSITION != 0;
    let menu = if by_position {
        root.0
    } else {
        match command_menu(env, root.0, first, last) {
            Ok(Some(menu)) => menu,
            Ok(None) => return fail(env, 1456),
            Err(8) => {
                adv_abi::warn_unavailable(
                    env,
                    API,
                    "radio command search exceeds the bounded 16384-item menu tree profile",
                );
                return fail(env, 8);
            }
            Err(error) => return fail(env, error),
        }
    };
    let items = get(env, menu).unwrap();
    let mut targets = Vec::new();
    if targets.try_reserve_exact(items.len()).is_err() {
        return fail(env, 8);
    }
    for (at, item) in items.iter().enumerate() {
        let value = if by_position { at as u32 } else { item.id };
        if value < first || value > last {
            continue;
        }
        // Command lookup is recursive, but all members must belong to the first
        // resolved menu. Duplicate IDs also edit only the ordinary lookup hit.
        if by_position
            || user(env)
                .menus
                .locate(&env.sys.objects, root.0, value, false)
                == Some((menu, at))
        {
            targets.push((at, value));
        }
    }
    if targets.is_empty() {
        return fail(env, 1456);
    }
    let found = targets.iter().any(|(_, value)| *value == checked);
    let missing = (targets.len() as u64) < u64::from(last) - u64::from(first) + 1;
    let items = get_mut(env, menu).unwrap();
    for (at, value) in targets {
        let item = &mut items[at];
        item.flags &= !CHECKED;
        if value == checked {
            item.flags |= RADIO | CHECKED;
        }
        // Native Windows keeps an unchecked item's previous RADIO type bit,
        // despite the current SDK prose describing its removal. Other type,
        // disabled/default bits, strings, bitmaps and data remain unchanged.
    }
    if !found {
        return fail(env, 87);
    }
    if missing {
        set_last_error(env, 87);
    }
    1
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(radio_ABI);
}
