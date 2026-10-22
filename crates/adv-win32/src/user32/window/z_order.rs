//! Topmost bands and owned-window ordering live in USER's existing z-order list.
use super::*;
const TOPMOST: u32 = 8;
fn is_top(user: &User, objects: &Objects, hwnd: u32) -> bool {
    user.windows
        .get(objects, &hwnd)
        .is_some_and(|w| w.ex_style & TOPMOST != 0)
}
fn band(user: &mut User, objects: &mut Objects, hwnd: u32, topmost: bool) {
    let mut changed = vec![hwnd];
    if !topmost {
        let mut owner = hwnd;
        for _ in 0..user.top.len() {
            owner = user.windows.get(objects, &owner).map_or(0, |w| w.owner);
            if owner == 0 || changed.contains(&owner) {
                break;
            }
            changed.push(owner);
        }
    }
    // Promotion includes owned windows; demotion includes owners and their
    // owned windows. A topmost owner cannot retain a non-topmost popup.
    loop {
        let before = changed.len();
        for &candidate in &user.top {
            if !changed.contains(&candidate)
                && user
                    .windows
                    .get(objects, &candidate)
                    .is_some_and(|w| changed.contains(&w.owner))
            {
                changed.push(candidate);
            }
        }
        if before == changed.len() {
            break;
        }
    }
    for hwnd in changed {
        if let Some(window) = user.windows.get_mut(objects, &hwnd) {
            if topmost {
                window.ex_style |= TOPMOST;
            } else {
                window.ex_style &= !TOPMOST;
            }
        }
    }
    let (top, ordinary): (Vec<_>, Vec<_>) = user
        .top
        .iter()
        .copied()
        .partition(|&h| is_top(user, objects, h));
    user.top = top.into_iter().chain(ordinary).collect();
}
pub(super) fn valid(user: &User, objects: &Objects, hwnd: u32, after: u32) -> bool {
    let Some(window) = user.windows.get(objects, &hwnd) else {
        return false;
    };
    if window.destroying {
        return false;
    }
    matches!(after, 0 | 1 | 0xffff_ffff | 0xffff_fffe)
        || user
            .windows
            .get(objects, &after)
            .is_some_and(|w| !w.destroying && w.parent == window.parent)
}
pub(super) fn place(user: &mut User, objects: &mut Objects, hwnd: u32, after: u32) -> bool {
    if !valid(user, objects, hwnd, after) {
        return false;
    }
    if after == hwnd {
        return true;
    }
    let window = user.windows.get(objects, &hwnd).unwrap();
    let (parent, owner) = (window.parent, window.owner);
    if parent != 0 {
        let Some(parent) = user.windows.get_mut(objects, &parent) else {
            return false;
        };
        if after == 0xffff_fffe {
            return true;
        }
        parent.children.retain(|&h| h != hwnd);
        let at = match after {
            0 | 0xffff_ffff => 0,
            1 => parent.children.len(),
            other => parent
                .children
                .iter()
                .position(|&h| h == other)
                .map_or(0, |i| i + 1),
        };
        parent.children.insert(at, hwnd);
        return true;
    }
    let previous = is_top(user, objects, hwnd);
    if after == 0xffff_fffe && !previous {
        return true;
    }
    let topmost = match after {
        0xffff_ffff => true,
        1 | 0xffff_fffe => false,
        0 => previous || is_top(user, objects, owner),
        other => {
            is_top(user, objects, other)
                && (previous
                    || user
                        .top
                        .iter()
                        .skip_while(|&&h| h != other)
                        .skip(1)
                        .any(|&h| h != hwnd && is_top(user, objects, h)))
        }
    };
    band(user, objects, hwnd, topmost);
    user.top.retain(|&h| h != hwnd);
    let at = match after {
        0 | 0xffff_ffff | 0xffff_fffe => {
            if topmost {
                0
            } else {
                user.top
                    .iter()
                    .take_while(|&&h| is_top(user, objects, h))
                    .count()
            }
        }
        1 => user.top.len(),
        other => user
            .top
            .iter()
            .position(|&h| h == other)
            .map_or(0, |i| i + 1),
    };
    user.top.insert(at, hwnd);
    // Owned popups always precede their owner, including when raising an owner.
    for _ in 0..user.top.len() {
        let misplaced = user.top.iter().enumerate().find_map(|(index, &h)| {
            let owner = user.windows.get(objects, &h)?.owner;
            let owner_at = user.top.iter().position(|&p| p == owner)?;
            (index > owner_at).then_some((index, owner_at))
        });
        let Some((index, owner_at)) = misplaced else {
            break;
        };
        let h = user.top.remove(index);
        user.top.insert(owner_at, h);
    }
    true
}
pub(super) fn raise(user: &mut User, objects: &mut Objects, hwnd: u32) {
    place(user, objects, hwnd, 0);
}
