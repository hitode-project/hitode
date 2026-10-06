//! Checked rectangle transforms use the ordinary signed Win32 RECT coordinates.
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;

use super::window::Rect;
use crate::util::set_last_error;

#[win32(user32, name = "InflateRect", level = implement)]
fn inflate(env: &mut Env<'_>, pointer: GuestPtr<Rect>, dx: i32, dy: i32) -> i32 {
    // Native NULL failure preserves LastError. Other unreadable/unwritable
    // pointers are checked before any coordinate can be modified.
    if pointer.is_null() {
        return 0;
    }
    if check_range(env.cpu, pointer.addr(), 16, Prot::READ | Prot::WRITE).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    let Some(rect) = Rect::read(env.cpu, pointer.addr()) else {
        set_last_error(env, 998);
        return 0;
    };
    // The authored x86 native oracle confirms modular arithmetic, including
    // INT_MIN deltas. Empty and inverted rectangles are not normalized/clamped.
    let coordinates = [
        rect.left.wrapping_sub(dx),
        rect.top.wrapping_sub(dy),
        rect.right.wrapping_add(dx),
        rect.bottom.wrapping_add(dy),
    ];
    let mut bytes = [0; 16];
    for (chunk, coordinate) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(coordinates) {
        chunk.copy_from_slice(&coordinate.to_le_bytes());
    }
    if env.cpu.write(pointer.addr(), &bytes).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    1
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(inflate_ABI);
}

#[cfg(test)]
mod tests;
