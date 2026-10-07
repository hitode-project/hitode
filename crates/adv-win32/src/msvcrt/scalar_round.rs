//! VC80/90 binary64 scalar rounding over the shared argument, x87-return and
//! errno owners. Bit operations make the result independent of host arithmetic
//! and the guest rounding direction; no guest FP control word is changed.
use super::{Env, Outcome, Registry, floating};
use adv_abi::{Api, guest::memory_failure, win32};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];
const SIGN: u64 = 1 << 63;
const FRACTION: u64 = (1 << 52) - 1;
const QUIET: u64 = 1 << 51;

#[derive(Clone, Copy)]
enum Operation {
    Floor,
    Ceil,
    Abs,
}

fn unavailable(env: &mut Env<'_>, reason: &'static str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    // The missing exception/status behavior cannot be represented by an
    // ordinary successful double result or an invented invalid callback.
    Outcome::Fail(format!("{api}: {reason}"))
}

fn calculate(env: &mut Env<'_>, input: f64, operation: Operation) -> Outcome {
    let bits = input.to_bits();
    let magnitude = bits & !SIGN;
    let exponent = (magnitude >> 52) as u16;
    let fraction = bits & FRACTION;
    if exponent == 0x7ff && fraction != 0 {
        if bits & QUIET == 0 {
            return unavailable(
                env,
                "signaling-NaN CRT invalid-operation status/exception handling is unavailable",
            );
        }
        // Measured default CRT domain handling: even fabs preserves a quiet
        // NaN's sign and payload, assigns EDOM, and preserves DOS/Win32 errors.
        // Custom CRT _matherr callbacks are outside the existing CRT profile.
        if let Err(error) = floating::errno_address(env).write(env.cpu, 33) {
            return memory_failure(error);
        }
        return Outcome::ReturnF64(input);
    }
    if matches!(operation, Operation::Abs) {
        if exponent == 0 && fraction != 0 {
            return unavailable(
                env,
                "subnormal fabs requires unavailable x87 denormal-operand status/exception handling",
            );
        }
        return Outcome::ReturnF64(f64::from_bits(magnitude));
    }
    // Includes infinities and already integral values at/above 2^52. Preserve
    // both zero signs without adding a value through a host floating operation.
    if magnitude == 0 || exponent >= 1023 + 52 {
        return Outcome::ReturnF64(input);
    }
    let negative = bits & SIGN != 0;
    if exponent < 1023 {
        let result = match (operation, negative) {
            (Operation::Floor, true) => (-1.0f64).to_bits(),
            (Operation::Floor, false) => 0,
            (Operation::Ceil, true) => SIGN,
            (Operation::Ceil, false) => 1.0f64.to_bits(),
            (Operation::Abs, _) => unreachable!(),
        };
        return Outcome::ReturnF64(f64::from_bits(result));
    }
    let fractional_bits = 1023 + 52 - exponent;
    let mask = (1u64 << fractional_bits) - 1;
    let mut result = bits & !mask;
    if bits & mask != 0
        && matches!(
            (operation, negative),
            (Operation::Floor, true) | (Operation::Ceil, false)
        )
    {
        result += 1u64 << fractional_bits;
    }
    Outcome::ReturnF64(f64::from_bits(result))
}

#[win32(msvcrt, name = "floor", level = implement)]
fn floor(env: &mut Env<'_>, input: f64) -> Outcome {
    calculate(env, input, Operation::Floor)
}

#[win32(msvcrt, name = "ceil", level = implement)]
fn ceil(env: &mut Env<'_>, input: f64) -> Outcome {
    calculate(env, input, Operation::Ceil)
}

#[win32(msvcrt, name = "fabs", level = implement)]
fn fabs(env: &mut Env<'_>, input: f64) -> Outcome {
    calculate(env, input, Operation::Abs)
}

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        for api in [floor_ABI, ceil_ABI, fabs_ABI] {
            registry.register(Api { module, ..api });
        }
    }
}

#[cfg(test)]
mod tests;
