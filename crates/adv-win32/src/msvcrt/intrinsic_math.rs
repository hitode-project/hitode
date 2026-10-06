//! Legacy x86 CRT intrinsics use ST0, not integer argument-stack slots.
//! This binary64 profile supports nearest rounding with masked x87 exceptions
//! and 53/64-bit precision. It does not emulate extended-precision/status bits
//! or exceptional CRT math handling. Rejected operands stay on the guest stack.
use super::{Env, Outcome, Registry};
use adv_abi::{Api, win32};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

#[derive(Clone, Copy)]
enum Operation {
    Tan,
    Sin,
    Cos,
    Sqrt,
}

fn unavailable(env: &mut Env<'_>, reason: &'static str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    Outcome::Fail(format!("{api}: {reason}"))
}

fn calculate(env: &mut Env<'_>, operation: Operation) -> Outcome {
    let Ok(control) = env.cpu.fpu_environment() else {
        return unavailable(env, "guest x87 control environment is unavailable");
    };
    if control.x87_control & 0x003f != 0x003f {
        return unavailable(env, "unmasked x87 math exceptions are unavailable");
    }
    if control.x87_control & 0x0c00 != 0 {
        return unavailable(env, "non-nearest x87 math rounding is unavailable");
    }
    if !matches!(control.x87_control & 0x0300, 0x0200 | 0x0300) {
        return unavailable(env, "x87 math requires 53-bit or 64-bit precision");
    }
    let Some(input) = env.cpu.pop_f64() else {
        return unavailable(env, "x87 math operand stack is empty");
    };
    let reason = if !input.is_finite() {
        Some("nonfinite x87 math operands are unavailable")
    } else if matches!(operation, Operation::Sqrt) && input < 0.0 {
        Some("negative square-root CRT exception handling is unavailable")
    } else {
        None
    };
    if let Some(reason) = reason {
        env.cpu.push_f64(input);
        return unavailable(env, reason);
    }
    let result = match operation {
        Operation::Tan => input.tan(),
        Operation::Sin => input.sin(),
        Operation::Cos => input.cos(),
        Operation::Sqrt => input.sqrt(),
    };
    // The machine restores a floating return to the same shared x87 owner and
    // applies the SDK cdecl(0) epilogue. It preserves the integer registers.
    Outcome::ReturnF64(result)
}

#[win32(msvcrt, name = "_CItan", level = implement)]
fn tangent(env: &mut Env<'_>) -> Outcome {
    calculate(env, Operation::Tan)
}
#[win32(msvcrt, name = "_CIsin", level = implement)]
fn sine(env: &mut Env<'_>) -> Outcome {
    calculate(env, Operation::Sin)
}
#[win32(msvcrt, name = "_CIcos", level = implement)]
fn cosine(env: &mut Env<'_>) -> Outcome {
    calculate(env, Operation::Cos)
}
#[win32(msvcrt, name = "_CIsqrt", level = implement)]
fn square_root(env: &mut Env<'_>) -> Outcome {
    calculate(env, Operation::Sqrt)
}

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        for api in [tangent_ABI, sine_ABI, cosine_ABI, square_root_ABI] {
            registry.register(Api { module, ..api });
        }
    }
}

#[cfg(test)]
mod tests;
