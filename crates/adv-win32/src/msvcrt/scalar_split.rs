//! Binary64 modf over the shared x86 argument, memory and x87-return owners.
//! Integer and fractional parts are assembled exactly from bits, independently
//! of host arithmetic and the guest rounding direction. No FP controls change.
use super::{Env, Outcome, Registry};
use adv_abi::{
    Api, GuestPtr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::Prot;

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];
const SIGN: u64 = 1 << 63;
const MANTISSA: u64 = (1 << 52) - 1;

fn parts(bits: u64) -> Result<(u64, u64), &'static str> {
    let sign = bits & SIGN;
    let exponent = ((bits >> 52) & 0x7ff) as u32;
    if exponent == 0x7ff && bits & MANTISSA != 0 {
        // Both VC80 and VC90 modf raise DE for qNaN; sNaN also raises IE.
        // Returning only the expected bits/EDOM would silently omit that state
        // and any unmasked exception or custom CRT math-error handling.
        return Err("NaN modf requires unavailable CRT domain and x87 exception/status handling");
    }
    if exponent == 0 && bits & MANTISSA != 0 {
        return Err(
            "subnormal modf requires unavailable x87 denormal-operand status/exception handling",
        );
    }
    if exponent < 1023 {
        return Ok((sign, bits));
    }
    // Every finite binary64 at/above 2^52 is integral. Native also produces
    // signed zero as the fractional part of either infinity.
    if exponent >= 1023 + 52 {
        return Ok((bits, sign));
    }
    let places = 1023 + 52 - exponent;
    let mask = (1u64 << places) - 1;
    let residue = bits & mask;
    let integer = bits & !mask;
    if residue == 0 {
        return Ok((integer, sign));
    }
    // residue * 2^(unbiased_exponent-52) is exact and at least 2^-52;
    // normalizing its leading bit cannot create a denormal or a rounding step.
    let leading = 63 - residue.leading_zeros();
    let fraction_exponent = exponent - 52 + leading;
    let fraction =
        sign | (u64::from(fraction_exponent) << 52) | ((residue << (52 - leading)) & MANTISSA);
    Ok((integer, fraction))
}

#[win32(msvcrt, name = "modf", level = implement)]
fn split(env: &mut Env<'_>, value: f64, integer: GuestPtr<f64>) -> Outcome {
    // The typed adapter has already snapshotted both argument DWORDs. Guard
    // the whole output before publishing even its first DWORD, including NULL.
    // Native NULL faults instead of invoking an invalid-parameter handler.
    if let Err(error) = check_range(env.cpu, integer.addr(), 8, Prot::WRITE) {
        return memory_failure(error);
    }
    let (whole, fraction) = match parts(value.to_bits()) {
        Ok(parts) => parts,
        Err(reason) => {
            let thunk = env.sys.thunks.get(env.thunk);
            let api = format!("{}!{}", thunk.module, thunk.name);
            adv_abi::warn_unavailable(env, &api, reason);
            return Outcome::Fail(format!("{api}: {reason}"));
        }
    };
    match integer.write(env.cpu, f64::from_bits(whole)) {
        Ok(()) => Outcome::ReturnF64(f64::from_bits(fraction)),
        Err(error) => memory_failure(error),
    }
}

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        registry.register(Api {
            module,
            ..split_ABI
        });
    }
}

#[cfg(test)]
mod tests;
