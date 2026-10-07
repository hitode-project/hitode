//! The VC90 no-C++-exception profile, backed by per-guest-thread observations.
use adv_abi::{Registry, win32};
use adv_machine::{Env, Outcome};

pub(super) fn register(registry: &mut Registry) {
    registry.register(uncaught_ABI);
    for module in ["msvcr80.dll", "msvcr90.dll"] {
        registry.register(adv_abi::Api {
            module,
            name: "__clean_type_info_names_internal",
            conv: adv_abi::Conv::Cdecl { argc: 1 },
            handler: clean_names,
            level: adv_abi::ImplementationLevel::Implement,
        });
    }
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

fn clean_names(env: &mut Env<'_>) -> Outcome {
    let root = env.arg(0);
    if let Err(error) = adv_abi::guest::check_range(env.cpu, root, 8, adv_cpu::backend::Prot::READ)
    {
        return adv_abi::guest::memory_failure(error);
    }
    let mut node = [0u8; 8];
    if let Err(error) = env.cpu.read(root, &mut node) {
        return adv_abi::guest::memory_failure(error);
    }
    if node != [0; 8] {
        adv_abi::warn_unavailable(
            env,
            "CRT __clean_type_info_names_internal",
            "nonempty type-info cache ownership is unavailable",
        );
        return Outcome::Fail("nonempty CRT type-info name cache".into());
    }
    // An empty x86 root owns no demangled names or linked nodes to release.
    Outcome::Return(0)
}

#[cfg(test)]
mod tests;
