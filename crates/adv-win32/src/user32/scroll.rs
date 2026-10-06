//! Numeric scrollbar state belongs to the USER window. SB_CTL calls are sent
//! to the control's guest procedure through the ordinary message mechanism.
use super::{message::send, user};
use crate::{Win32, util::set_last_error};
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

const RANGE: u32 = 1;
const PAGE: u32 = 2;
const POS: u32 = 4;
const TRACK: u32 = 16;

#[derive(Clone, Copy)]
pub(crate) struct ScrollState {
    minimum: i32,
    maximum: i32,
    page: u32,
    position: i32,
}
impl Default for ScrollState {
    fn default() -> Self {
        Self {
            minimum: 0,
            maximum: 100,
            page: 0,
            position: 0,
        }
    }
}
impl ScrollState {
    fn clamp(&mut self) {
        let count = (i64::from(self.maximum) - i64::from(self.minimum) + 1) as u64;
        self.page = u64::from(self.page).min(count) as u32;
        let last = i64::from(self.maximum) - i64::from(self.page.saturating_sub(1));
        self.position = i64::from(self.position).clamp(i64::from(self.minimum), last) as i32;
    }
}

fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
fn bar(env: &Env<'_>, hwnd: u32, which: i32) -> Result<Option<usize>, u32> {
    if !user(env).windows.contains_key(&env.sys.objects, &hwnd) {
        return Err(1400);
    }
    match which {
        0 | 1 => Ok(Some(which as usize)),
        2 => Ok(None),
        _ => Err(87),
    }
}
fn state(env: &Env<'_>, hwnd: u32, index: usize) -> Option<ScrollState> {
    Some(
        user(env)
            .windows
            .get(&env.sys.objects, &hwnd)?
            .control
            .scrolls?[index],
    )
}
fn update(
    env: &mut Env<'_>,
    hwnd: u32,
    index: usize,
    range: Option<(i32, i32)>,
    page: Option<u32>,
    position: Option<i32>,
    redraw: bool,
) -> i32 {
    let window = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
        .unwrap();
    let values = window
        .control
        .scrolls
        .get_or_insert_with(|| [ScrollState::default(); 2]);
    let value = &mut values[index];
    if let Some((minimum, maximum)) = range {
        value.minimum = minimum;
        value.maximum = maximum.max(minimum);
    }
    if let Some(page) = page {
        value.page = page;
    }
    if let Some(position) = position {
        value.position = position;
    }
    value.clamp();
    if redraw {
        window.needs_paint = true;
    }
    value.position
}
fn header(env: &Env<'_>, pointer: u32) -> Option<(u32, u32)> {
    let mask = pointer.checked_add(4)?;
    let size = env.cpu.read_u32(pointer).ok()?;
    if !matches!(size, 24 | 28) {
        return None;
    }
    Some((size, env.cpu.read_u32(mask).ok()?))
}

#[win32(user32, name = "GetScrollPos", level = state)]
fn get_pos(env: &mut Env<'_>, hwnd: Handle, which: i32) -> Outcome {
    let index = match bar(env, hwnd.0, which) {
        Ok(Some(index)) => index,
        Ok(None) => return send(env, hwnd.0, 0xe1, 0, 0, |_, value| Outcome::Return(value)),
        Err(error) => return fail(env, error),
    };
    match state(env, hwnd.0, index) {
        Some(value) => Outcome::Return(value.position as u32),
        None => fail(env, 1447),
    }
}

#[win32(user32, name = "SetScrollPos", level = state)]
fn set_pos(env: &mut Env<'_>, hwnd: Handle, which: i32, position: i32, redraw: i32) -> Outcome {
    let index = match bar(env, hwnd.0, which) {
        Ok(Some(index)) => index,
        Ok(None) => {
            return send(
                env,
                hwnd.0,
                0xe0,
                position as u32,
                redraw as u32,
                |_, value| Outcome::Return(value),
            );
        }
        Err(error) => return fail(env, error),
    };
    let old = state(env, hwnd.0, index).unwrap_or_default().position;
    update(env, hwnd.0, index, None, None, Some(position), redraw != 0);
    Outcome::Return(old as u32)
}

#[win32(user32, name = "SetScrollInfo", level = state)]
fn set_info(
    env: &mut Env<'_>,
    hwnd: Handle,
    which: i32,
    pointer: GuestPtr<()>,
    redraw: i32,
) -> Outcome {
    let index = match bar(env, hwnd.0, which) {
        Ok(Some(index)) => index,
        Ok(None) => {
            return send(
                env,
                hwnd.0,
                0xe9,
                redraw as u32,
                pointer.addr(),
                |_, value| Outcome::Return(value),
            );
        }
        Err(error) => return fail(env, error),
    };
    let Some((_, mask)) = header(env, pointer.addr()) else {
        return Outcome::Return(0);
    };
    let mut bytes = [0u8; 24];
    if env.cpu.read(pointer.addr(), &mut bytes).is_err() {
        return fail(env, 998);
    }
    if mask & (RANGE | PAGE | POS) == 0 {
        return match state(env, hwnd.0, index) {
            Some(value) => Outcome::Return(value.position as u32),
            None => fail(env, 1447),
        };
    }
    let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let range = (mask & RANGE != 0).then(|| (word(8) as i32, word(12) as i32));
    let page = (mask & PAGE != 0).then(|| word(16));
    let position = (mask & POS != 0).then(|| word(20) as i32);
    Outcome::Return(update(env, hwnd.0, index, range, page, position, redraw != 0) as u32)
}

#[win32(user32, name = "GetScrollInfo", level = state)]
fn get_info(env: &mut Env<'_>, hwnd: Handle, which: i32, pointer: GuestPtr<()>) -> Outcome {
    let index = match bar(env, hwnd.0, which) {
        Ok(Some(index)) => index,
        Ok(None) => {
            return send(env, hwnd.0, 0xea, 0, pointer.addr(), |_, value| {
                Outcome::Return(value)
            });
        }
        Err(error) => return fail(env, error),
    };
    let Some((_, mask)) = header(env, pointer.addr()) else {
        return Outcome::Return(0);
    };
    let Some(value) = state(env, hwnd.0, index) else {
        return fail(env, 1447);
    };
    if mask & (RANGE | PAGE | POS | TRACK) == 0 {
        return Outcome::Return(0);
    }
    let fields = [
        (RANGE, 8, value.minimum as u32),
        (RANGE, 12, value.maximum as u32),
        (PAGE, 16, value.page),
        (POS, 20, value.position as u32),
        (TRACK, 24, value.position as u32),
    ];
    // Check every requested field before publishing any of them. Windows also
    // accepts the legacy size 24, including a requested track field at +24.
    for &(flag, offset, _) in &fields {
        if mask & flag != 0
            && pointer.addr().checked_add(offset).is_none_or(|address| {
                adv_abi::guest::check_range(env.cpu, address, 4, Prot::WRITE).is_err()
            })
        {
            return fail(env, 998);
        }
    }
    for (flag, offset, value) in fields {
        if mask & flag != 0 {
            let _ = env.cpu.write_u32(pointer.addr() + offset, value);
        }
    }
    Outcome::Return(1)
}

#[win32(user32, name = "GetScrollRange", level = state)]
fn get_range(
    env: &mut Env<'_>,
    hwnd: Handle,
    which: i32,
    minimum: GuestPtr<i32>,
    maximum: GuestPtr<i32>,
) -> Outcome {
    let index = match bar(env, hwnd.0, which) {
        Ok(Some(index)) => index,
        Ok(None) => {
            return send(
                env,
                hwnd.0,
                0xe3,
                minimum.addr(),
                maximum.addr(),
                |_, value| Outcome::Return(value),
            );
        }
        Err(error) => return fail(env, error),
    };
    if adv_abi::guest::check_range(env.cpu, minimum.addr(), 4, Prot::WRITE).is_err()
        || adv_abi::guest::check_range(env.cpu, maximum.addr(), 4, Prot::WRITE).is_err()
    {
        return fail(env, 998);
    }
    let (low, high) =
        state(env, hwnd.0, index).map_or((0, 0), |value| (value.minimum, value.maximum));
    let _ = minimum.write(env.cpu, low);
    let _ = maximum.write(env.cpu, high);
    Outcome::Return(1)
}

#[win32(user32, name = "SetScrollRange", level = state)]
fn set_range(
    env: &mut Env<'_>,
    hwnd: Handle,
    which: i32,
    minimum: i32,
    maximum: i32,
    redraw: i32,
) -> Outcome {
    let index = match bar(env, hwnd.0, which) {
        Ok(Some(index)) => index,
        Ok(None) => {
            return send(
                env,
                hwnd.0,
                if redraw != 0 { 0xe6 } else { 0xe2 },
                minimum as u32,
                maximum as u32,
                |_, value| Outcome::Return(value),
            );
        }
        Err(error) => return fail(env, error),
    };
    if minimum > maximum || i64::from(maximum) - i64::from(minimum) > i64::from(i32::MAX) {
        return fail(env, 1448);
    }
    update(
        env,
        hwnd.0,
        index,
        Some((minimum, maximum)),
        None,
        None,
        redraw != 0,
    );
    Outcome::Return(1)
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        get_pos_ABI,
        set_pos_ABI,
        set_info_ABI,
        get_info_ABI,
        get_range_ABI,
        set_range_ABI,
    ] {
        registry.register(api);
    }
}

#[cfg(test)]
mod tests;
