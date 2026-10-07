//! Binary64 power-of-two scaling using the existing CRT scale helper.
//! Normal finite inputs/results and signed zero are supported; exceptional
//! range/denormal/NaN and unmasked or directed-rounding profiles fail explicitly.
use super::{Env, Outcome, Registry, scale};
use adv_abi::{Api, win32};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

fn unavailable(env: &mut Env<'_>, reason: &'static str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    Outcome::Fail(format!("{api}: {reason}"))
}

#[win32(msvcrt, name = "ldexp", level = implement)]
fn exponent(env: &mut Env<'_>, value: f64, power: i32) -> Outcome {
    let Ok(mut state) = env.cpu.capture_architecture() else {
        return unavailable(env, "ldexp guest floating architecture is unavailable");
    };
    let control = state.x87.control;
    if control & 0x3f != 0x3f
        || control & 0x0c00 != 0
        || !matches!(control & 0x0300, 0x0200 | 0x0300)
    {
        return unavailable(
            env,
            "ldexp requires masked exceptions, nearest rounding and PC53/64",
        );
    }
    let return_slot = state.x87.top().wrapping_sub(1) & 7;
    if state.x87.physical_tag(return_slot) != 3 {
        return unavailable(
            env,
            "ldexp x87 return stack-overflow handling is unavailable",
        );
    }
    if !value.is_finite() || value.is_subnormal() {
        return unavailable(
            env,
            "ldexp nonfinite/denormal operand CRT exception handling is unavailable",
        );
    }
    let result = scale(value, power);
    if !result.is_finite() || result.is_subnormal() || (result == 0. && value != 0.) {
        return unavailable(
            env,
            "ldexp overflow/underflow CRT errno, math-error and FP status handling is unavailable",
        );
    }
    // The authored normal VC80/90 probe leaves comparison-to-zero condition
    // codes; all three CRTs clear C1. No exception flags or lower payloads are
    // changed here, and rejected calls never commit the local snapshot.
    state.x87.status &= !0x0200;
    if env.sys.thunks.get(env.thunk).module != "msvcrt.dll" {
        state.x87.status &= !0x4500;
        if value == 0. {
            state.x87.status |= 0x4000;
        } else if value > 0. {
            state.x87.status |= 0x0100;
        }
    }
    if let Err(error) = env.cpu.restore_architecture(&state) {
        return Outcome::Fail(format!("ldexp: architectural commit failed: {error}"));
    }
    Outcome::ReturnF64(result)
}

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        registry.register(Api {
            module,
            ..exponent_ABI
        });
    }
}

#[cfg(test)]
mod tests;
