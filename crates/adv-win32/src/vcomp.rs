//! The single-processor OpenMP profile executes each outlined region on its caller.
//! ABI references: Wine's vcomp export declarations, verified against native vcomp140.
use adv_abi::{Api, ImplementationLevel, Registry};
use adv_cpu::backend::{GuestExt, Prot, Reg};
use adv_machine::{Conv, Env, Outcome};

fn fork(env: &mut Env<'_>) -> Outcome {
    let count = env.arg(1);
    let target = env.arg(2);
    if count > 128 || target == 0 {
        adv_abi::warn_unavailable(
            env,
            "vcomp140.dll!_vcomp_fork",
            "invalid outlined callback or argument count",
        );
        return Outcome::Fail("invalid OpenMP outlined region".into());
    }
    let Some(arguments) = env.cpu.reg(Reg::Esp).checked_add(16) else {
        return Outcome::Fail("OpenMP argument range overflow".into());
    };
    if let Err(error) = adv_abi::guest::check_range(env.cpu, arguments, count * 4, Prot::READ) {
        return adv_abi::guest::memory_failure(error);
    }
    let mut args = Vec::with_capacity(count as usize);
    for index in 0..count {
        match env.cpu.read_u32(arguments + index * 4) {
            Ok(value) => args.push(value),
            Err(error) => return adv_abi::guest::memory_failure(error),
        }
    }
    // A one-thread team covers every iteration, including nested regions.
    Outcome::call(target, args, |_, _| Outcome::Return(0))
}

fn static_range(env: &mut Env<'_>) -> Outcome {
    let (first, last, begin, end) = (env.arg(0), env.arg(1), env.arg(4), env.arg(5));
    for pointer in [begin, end] {
        if let Err(error) = adv_abi::guest::check_range(env.cpu, pointer, 4, Prot::WRITE) {
            return adv_abi::guest::memory_failure(error);
        }
    }
    for (pointer, value) in [(begin, first), (end, last)] {
        if let Err(error) = env.cpu.write_u32(pointer, value) {
            return adv_abi::guest::memory_failure(error);
        }
    }
    Outcome::Return(0)
}

pub(crate) fn register(reg: &mut Registry) {
    for (name, conv, handler) in [
        (
            "_vcomp_fork",
            Conv::CdeclVariadic { fixed: 3 },
            fork as adv_machine::Handler,
        ),
        (
            "_vcomp_for_static_simple_init",
            Conv::Cdecl { argc: 6 },
            static_range,
        ),
        (
            "_vcomp_for_static_end",
            Conv::Cdecl { argc: 0 },
            (|_| Outcome::Return(0)) as adv_machine::Handler,
        ),
    ] {
        reg.register(Api {
            module: "vcomp140.dll",
            name,
            conv,
            handler,
            env_leaf: None,
            level: ImplementationLevel::Implement,
        });
    }
}
