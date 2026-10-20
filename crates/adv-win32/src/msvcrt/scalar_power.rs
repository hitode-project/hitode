//! x86 _CIpow and _CIfmod take their second operand in ST0 and the first in
//! ST1, with no integer-stack arguments. This binary64 profile uses the existing architectural/x87 owner
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
        return Err("nonfinite x87 CRT math-error handling is unavailable");
    }
    if value.is_subnormal() {
        return Err("subnormal x87 CRT operands and denormal/status handling are unavailable");
    }
    if x87::encode_f64(value) != register.0 {
        return Err("extended x87 CRT operands outside exact binary64 are unavailable");
    }
    Ok(value)
}

/// `base` to the `exponent` within the supported finite, normal profile.
fn checked_power(base: f64, exponent: f64) -> Result<f64, &'static str> {
    if base < 0.0 && exponent.fract() != 0.0 {
        return Err("negative-base fractional power domain handling is unavailable");
    }
    if base == 0.0 && (base.is_sign_negative() || exponent <= 0.0) {
        return Err("signed-zero or nonpositive zero-base power handling is unavailable");
    }
    let result = base.powf(exponent);
    if !result.is_finite() {
        return Err("power overflow/domain CRT error handling is unavailable");
    }
    if result.is_subnormal() || (result == 0.0 && base != 0.0) {
        return Err("power underflow/subnormal CRT error handling is unavailable");
    }
    Ok(result)
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
    let result = match checked_power(base, exponent) {
        Ok(result) => result,
        Err(reason) => return unavailable(env, reason),
    };
    // Measured VC80/90 normal results preserve exception/condition flags, CW,
    // errno/DOS/LastError and lower registers; only TOP/tags lose two inputs.
    // ReturnF64 pushes the single result through the same machine FP bridge.
    if let Err(error) = env.cpu.restore_architecture(&snapshot) {
        return Outcome::Fail(format!("_CIpow: architectural commit failed: {error}"));
    }
    Outcome::ReturnF64(result)
}

#[win32(msvcrt, name = "_CIfmod", level = implement)]
fn remainder(env: &mut Env<'_>) -> Outcome {
    let Ok(mut snapshot) = env.cpu.capture_architecture() else {
        return unavailable(env, "guest x87 architectural state is unavailable");
    };
    if snapshot.x87.control & 0x003f != 0x003f {
        return unavailable(env, "unmasked _CIfmod x87 exceptions are unavailable");
    }
    let (Some(divisor), Some(dividend)) = (snapshot.x87.pop(), snapshot.x87.pop()) else {
        return unavailable(env, "_CIfmod requires two populated x87 operands");
    };
    let (dividend, divisor) = match (operand(dividend), operand(divisor)) {
        (Ok(dividend), Ok(divisor)) => (dividend, divisor),
        (Err(reason), _) | (_, Err(reason)) => return unavailable(env, reason),
    };
    if divisor == 0.0 {
        return unavailable(env, "zero-divisor _CIfmod domain handling is unavailable");
    }
    // The remainder is exact, so rounding and precision control cannot change it.
    if let Err(error) = env.cpu.restore_architecture(&snapshot) {
        return Outcome::Fail(format!("_CIfmod: architectural commit failed: {error}"));
    }
    Outcome::ReturnF64(dividend % divisor)
}

#[win32(msvcrt, name = "pow", level = implement)]
fn double_power(env: &mut Env<'_>, base: f64, exponent: f64) -> Outcome {
    for value in [base, exponent] {
        if !value.is_finite() || value.is_subnormal() {
            return unavailable(env, "nonfinite or subnormal pow operands are unavailable");
        }
    }
    match checked_power(base, exponent) {
        Ok(result) => Outcome::ReturnF64(result),
        Err(reason) => unavailable(env, reason),
    }
}

#[win32(msvcrt, name = "_hypot", level = implement)]
fn hypotenuse(env: &mut Env<'_>, x: f64, y: f64) -> Outcome {
    let result = x.hypot(y);
    if !x.is_finite() || !y.is_finite() || !result.is_finite() || result.is_subnormal() {
        return unavailable(
            env,
            "_hypot range and nonfinite CRT error handling is unavailable",
        );
    }
    Outcome::ReturnF64(result)
}

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        for api in [power_ABI, remainder_ABI, double_power_ABI, hypotenuse_ABI] {
            registry.register(Api { module, ..api });
        }
    }
}

#[cfg(test)]
mod tests;
