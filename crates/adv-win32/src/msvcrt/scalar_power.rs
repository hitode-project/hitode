//! x86 _CIpow takes exponent in ST0 and base in ST1, with no integer-stack
//! arguments. This binary64 profile uses the existing architectural/x87 owner
//! to commit two consumed operands only after all input/result checks succeed.
//! It supports masked exceptions, nearest rounding and PC53/64. Host powf is
//! used only within that profile; extended-precision CRT bit identity, custom
//! math-error handling and exceptional range/domain inputs remain unavailable.
use super::{Env, Outcome, Registry};
use adv_abi::{Api, win32};
use adv_cpu::{X87Register, backend::x87};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

fn unavailable(env: &mut Env<'_>, reason: &'static str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    Outcome::Fail(format!("{api}: {reason}"))
}

fn operand(register: X87Register) -> Result<f64, &'static str> {
    let value = x87::decode_f64(&register.0);
    if !value.is_finite() {
        return Err("nonfinite _CIpow CRT math-error handling is unavailable");
    }
    if value.is_subnormal() {
        return Err("subnormal _CIpow operands and denormal/status handling are unavailable");
    }
    if x87::encode_f64(value) != register.0 {
        return Err("extended _CIpow operands outside exact binary64 are unavailable");
    }
    Ok(value)
}

#[win32(msvcrt, name = "_CIpow", level = implement)]
fn power(env: &mut Env<'_>) -> Outcome {
    let Ok(mut snapshot) = env.cpu.capture_architecture() else {
        return unavailable(env, "guest x87 architectural state is unavailable");
    };
    let control = snapshot.x87.control;
    if control & 0x003f != 0x003f {
        return unavailable(env, "unmasked _CIpow x87 exceptions are unavailable");
    }
    if control & 0x0c00 != 0 {
        return unavailable(env, "non-nearest _CIpow x87 rounding is unavailable");
    }
    if !matches!(control & 0x0300, 0x0200 | 0x0300) {
        return unavailable(env, "_CIpow requires 53-bit or 64-bit x87 precision");
    }
    // Pop only the local snapshot. Any validation or math failure preserves
    // live tags, TOP, sticky/condition flags and every original 80-bit payload.
    let (Some(exponent), Some(base)) = (snapshot.x87.pop(), snapshot.x87.pop()) else {
        return unavailable(env, "_CIpow requires two populated x87 operands");
    };
    let (base, exponent) = match (operand(base), operand(exponent)) {
        (Ok(base), Ok(exponent)) => (base, exponent),
        (Err(reason), _) | (_, Err(reason)) => return unavailable(env, reason),
    };
    if base < 0.0 && exponent.fract() != 0.0 {
        return unavailable(
            env,
            "negative-base fractional _CIpow domain handling is unavailable",
        );
    }
    if base == 0.0 && (base.is_sign_negative() || exponent <= 0.0) {
        return unavailable(
            env,
            "signed-zero or nonpositive zero-base _CIpow handling is unavailable",
        );
    }
    let result = base.powf(exponent);
    if !result.is_finite() {
        return unavailable(
            env,
            "_CIpow overflow/domain CRT error handling is unavailable",
        );
    }
    if result.is_subnormal() || (result == 0.0 && base != 0.0) {
        return unavailable(
            env,
            "_CIpow underflow/subnormal CRT error handling is unavailable",
        );
    }
    // Measured VC80/90 normal results preserve exception/condition flags, CW,
    // errno/DOS/LastError and lower registers; only TOP/tags lose two inputs.
    // ReturnF64 pushes the single result through the same machine FP bridge.
    if let Err(error) = env.cpu.restore_architecture(&snapshot) {
        return Outcome::Fail(format!("_CIpow: architectural commit failed: {error}"));
    }
    Outcome::ReturnF64(result)
}

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        registry.register(Api {
            module,
            ..power_ABI
        });
    }
}

#[cfg(test)]
mod tests;
