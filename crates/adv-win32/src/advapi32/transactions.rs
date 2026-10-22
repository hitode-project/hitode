//! Invalid transaction fronts preserve real registry fast paths and failure outputs.
//! The closure creates no transaction objects or inherited transaction handles.
use adv_abi::{Registry, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

const INVALID_TRANSACTION: u32 = 6700;
fn invalid_transaction(handle: u32) -> u32 {
    if matches!(handle, 0 | u32::MAX) {
        INVALID_TRANSACTION
    } else {
        6
    }
}
fn open(env: &mut Env<'_>) -> Outcome {
    if env.arg(6) != 0 {
        return Outcome::Return(87);
    }
    let handle = env.arg(5);
    if matches!(handle, 0 | u32::MAX) {
        return Outcome::Return(INVALID_TRANSACTION);
    }
    // The predefined-key empty-name fast path does not consume a transaction,
    // even for an invalid nonzero transaction handle. Reuse the real key owner.
    if matches!(env.arg(0), 0x80000000..=0x80000003 | 0x80000005) {
        match super::name(env, env.arg(1), true, true) {
            Ok(name) if name.is_empty() => return super::open_key(env, true, false, false),
            Ok(_) => (),
            Err(error) => return Outcome::Return(error),
        }
    }
    let output = env.arg(4);
    if check_range(env.cpu, output, 4, Prot::WRITE).is_err() {
        return Outcome::Return(87);
    }
    let _ = env.cpu.write_u32(output, 0);
    Outcome::Return(invalid_transaction(handle))
}
fn create(env: &mut Env<'_>) -> Outcome {
    if env.arg(2) != 0 || env.arg(10) != 0 {
        return Outcome::Return(87);
    }
    let handle = env.arg(9);
    if matches!(handle, 0 | u32::MAX) {
        return Outcome::Return(INVALID_TRANSACTION);
    }
    let output = env.arg(7);
    if check_range(env.cpu, output, 4, Prot::WRITE).is_err() {
        return Outcome::Return(87);
    }
    let _ = env.cpu.write_u32(output, 0);
    Outcome::Return(invalid_transaction(handle))
}
fn delete(env: &mut Env<'_>) -> Outcome {
    if env.arg(3) != 0 || env.arg(5) != 0 {
        return Outcome::Return(87);
    }
    Outcome::Return(invalid_transaction(env.arg(4)))
}
pub(super) fn register(registry: &mut Registry) {
    for (name, count, handler) in [
        ("RegOpenKeyTransactedW", 7, open as adv_machine::Handler),
        ("RegCreateKeyTransactedW", 11, create),
        ("RegDeleteKeyTransactedW", 6, delete),
    ] {
        crate::stdcall(registry, "advapi32.dll", name, count, handler);
    }
}

#[cfg(test)]
mod tests;
