//! Brush alignment belongs to the common DC, including SaveDC/RestoreDC.
//! The current solid brushes do not depend on alignment when drawing.
use super::{GdiObj, get, get_mut};
use crate::util::set_last_error;
use adv_abi::{Env, GuestPtr, Handle, Outcome, Registry, types::POINT, win32};
use adv_cpu::backend::Prot;

pub(super) fn register(registry: &mut Registry) {
    registry.register(set_origin_ABI);
    registry.register(get_origin_ABI);
}

#[win32(gdi32, name = "SetBrushOrgEx", level = state)]
fn set_origin(env: &mut Env<'_>, dc: Handle, x: i32, y: i32, previous: GuestPtr<POINT>) -> Outcome {
    let Some(GdiObj::Dc { modes, .. }) = get(env, dc.0) else {
        set_last_error(env, 6);
        return Outcome::Return(0);
    };
    let [old_x, old_y] = modes.brush_origin;
    // Native preserves the signed coordinates in the DC; modulus belongs to
    // pattern realization, not this state setter. Output precedes mutation.
    if !previous.is_null()
        && (adv_abi::guest::check_range(env.cpu, previous.addr(), 8, Prot::WRITE).is_err()
            || previous
                .write(env.cpu, POINT { x: old_x, y: old_y })
                .is_err())
    {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let Some(GdiObj::Dc { modes, .. }) = get_mut(env, dc.0) else {
        unreachable!("checked DC")
    };
    modes.brush_origin = [x, y];
    Outcome::Return(1)
}

#[win32(gdi32, name = "GetBrushOrgEx", level = state)]
fn get_origin(env: &mut Env<'_>, dc: Handle, output: GuestPtr<POINT>) -> Outcome {
    let Some(GdiObj::Dc { modes, .. }) = get(env, dc.0) else {
        set_last_error(env, 87);
        return Outcome::Return(0);
    };
    let [x, y] = modes.brush_origin;
    if output.is_null()
        || adv_abi::guest::check_range(env.cpu, output.addr(), 8, Prot::WRITE).is_err()
        || output.write(env.cpu, POINT { x, y }).is_err()
    {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    Outcome::Return(1)
}

#[cfg(test)]
mod tests;
