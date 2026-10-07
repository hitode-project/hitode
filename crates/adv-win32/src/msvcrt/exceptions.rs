//! The VC90 no-C++-exception profile, backed by per-guest-thread observations.
use adv_abi::{Registry, win32};
use adv_machine::{Env, Outcome};

pub(super) fn register(registry: &mut Registry) {
    registry.register(uncaught_ABI);
}

#[win32(msvcr90, name = "__uncaught_exception", level = implement)]
fn uncaught(env: &mut Env<'_>) -> Outcome {
    if let Some(reason) = crate::kernel32::uncaught_query_blocker(env) {
        adv_abi::warn_unavailable(env, "msvcr90.dll!__uncaught_exception", reason);
        return Outcome::Fail(format!("__uncaught_exception: {reason}"));
    }
    // Native x86 VC90 reads its thread's processingThrow counter and returns
    // bool in AL. No C++ throw/catch frontend is installed; only a clean thread
    // with no observed exception work has a provably zero counter.
    Outcome::Return(0)
}

#[cfg(test)]
mod tests;
