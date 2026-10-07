//! x86 _CIlog10/_CIlog consume ST0 and return one x87 value, with no integer-stack
//! arguments. Masked exceptions, nearest rounding and PC53/64 are supported.
//! Exact normal binary64 inputs use host log10; the measured normal extended
//! volume domain [0.001, 1) is first rounded to binary64. Native VC80/90 normal
//! results agree within two binary64 ULPs in the authored probe. This is not an
//! extended-precision bit identity or exceptional CRT math-error profile.
//! _CIlog is limited to positive normal inputs exactly representable in
//! binary64, including the measured binary32 FFT lengths and the constant 2.
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
    let exponent = register.sign_exponent() & 0x7fff;
    if exponent == 0 || exponent == 0x7fff || register.significand() & (1 << 63) == 0 {
        return Err("_CIlog10 requires a positive normal finite x87 operand");
    }
    let value = x87::decode_f64(&register.0);
    if !value.is_finite() || value <= 0.0 || value.is_subnormal() {
        return Err("_CIlog10 domain, nonfinite and denormal CRT handling is unavailable");
    }
    if x87::encode_f64(value) != register.0 && !(0.001..1.0).contains(&value) {
        // In particular, do not round a nonexact value next to 1 to an exact
        // zero logarithm. Only the measured normalized volume domain is added
        // to the exact binary64 profile; extended ranges remain unavailable.
        return Err(
            "extended _CIlog10 outside the measured normalized volume domain is unavailable",
        );
    }
    Ok(value)
}

#[win32(msvcrt, name = "_CIlog10", level = implement)]
fn logarithm(env: &mut Env<'_>) -> Outcome {
    let Ok(mut snapshot) = env.cpu.capture_architecture() else {
        return unavailable(env, "guest x87 architectural state is unavailable");
    };
    let control = snapshot.x87.control;
    if control & 0x003f != 0x003f {
        return unavailable(env, "unmasked _CIlog10 x87 exceptions are unavailable");
    }
    if control & 0x0c00 != 0 {
        return unavailable(env, "non-nearest _CIlog10 x87 rounding is unavailable");
    }
    if !matches!(control & 0x0300, 0x0200 | 0x0300) {
        return unavailable(env, "_CIlog10 requires 53-bit or 64-bit x87 precision");
    }
    // Pop only the local snapshot: all validation failures leave the complete
    // live 80-bit payloads, tags, TOP, controls and sticky flags untouched.
    let Some(input) = snapshot.x87.pop() else {
        return unavailable(env, "_CIlog10 requires one populated x87 operand");
    };
    let value = match operand(input) {
        Ok(value) => value,
        Err(reason) => return unavailable(env, reason),
    };
    let result = value.log10();
    if !result.is_finite() {
        return unavailable(env, "_CIlog10 exceptional result handling is unavailable");
    }
    // The authored VC80/90 probe clears C1 on successful return, including a
    // pre-existing FDIV rounding flag. Other flags, CW and error cells survive.
    snapshot.x87.status &= !0x0200;
    if let Err(error) = env.cpu.restore_architecture(&snapshot) {
        return Outcome::Fail(format!("_CIlog10: architectural commit failed: {error}"));
    }
    Outcome::ReturnF64(result)
}

#[win32(msvcrt, name = "_CIlog", level = implement)]
fn natural_logarithm(env: &mut Env<'_>) -> Outcome {
    let Ok(mut snapshot) = env.cpu.capture_architecture() else {
        return unavailable(env, "guest x87 architectural state is unavailable");
    };
    let control = snapshot.x87.control;
    if control & 0x003f != 0x003f {
        return unavailable(env, "unmasked _CIlog x87 exceptions are unavailable");
    }
    if control & 0x0c00 != 0 {
        return unavailable(env, "non-nearest _CIlog x87 rounding is unavailable");
    }
    if !matches!(control & 0x0300, 0x0200 | 0x0300) {
        return unavailable(env, "_CIlog requires 53-bit or 64-bit x87 precision");
    }
    // As for _CIlog10, only the local architectural snapshot is consumed.
    let Some(input) = snapshot.x87.pop() else {
        return unavailable(env, "_CIlog requires one populated x87 operand");
    };
    let value = x87::decode_f64(&input.0);
    if !value.is_finite() || value <= 0.0 || value.is_subnormal() {
        return unavailable(
            env,
            "_CIlog domain, nonfinite and denormal CRT handling is unavailable",
        );
    }
    if x87::encode_f64(value) != input.0 {
        return unavailable(
            env,
            "extended _CIlog operands outside exact binary64 are unavailable",
        );
    }
    let result = value.ln();
    if !result.is_finite() {
        return unavailable(env, "_CIlog exceptional result handling is unavailable");
    }
    // The VC80/90 normal result profile preserves CW, sticky flags, error
    // cells and lower registers. Retire C1 at the successful FP return only.
    snapshot.x87.status &= !0x0200;
    if let Err(error) = env.cpu.restore_architecture(&snapshot) {
        return Outcome::Fail(format!("_CIlog: architectural commit failed: {error}"));
    }
    Outcome::ReturnF64(result)
}

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        registry.register(Api {
            module,
            ..logarithm_ABI
        });
        registry.register(Api {
            module,
            ..natural_logarithm_ABI
        });
    }
}

#[cfg(test)]
mod tests;
