//! The process PEB lock is a guest critical section shared with Win32 callers.
use adv_abi::{Registry, win32};
use adv_cpu::backend::{Backend, GuestExt, MemFault, Prot};
use adv_machine::{
    Env, Machine, Outcome,
    layout::{PEB_ADDR, peb},
};

use crate::kernel32::{CS_SIZE, enter_cs, initialize_cs, leave_cs};

pub(crate) fn install<B: Backend>(machine: &mut Machine<B>) -> Result<(), MemFault> {
    // Process infrastructure has VM lifetime and is not a caller-owned heap block.
    let section = machine.sys.vm.alloc(&mut machine.cpu, CS_SIZE, Prot::RW)?;
    let result = initialize_cs(&mut machine.cpu, section).and_then(|()| {
        machine
            .cpu
            .write_u32(PEB_ADDR + peb::FAST_PEB_LOCK, section)
    });
    if let Err(error) = result {
        let _ = machine.sys.vm.free(&mut machine.cpu, section);
        return Err(error);
    }
    Ok(())
}

fn section(env: &Env<'_>) -> Result<u32, String> {
    env.cpu
        .read_u32(PEB_ADDR + peb::FAST_PEB_LOCK)
        .map_err(|error| format!("cannot read process PEB lock: {error:?}"))
        .and_then(|section| {
            if section == 0 {
                Err("process PEB lock is not initialized".into())
            } else {
                Ok(section)
            }
        })
}

#[win32(ntdll, name = "RtlAcquirePebLock", level = implement)]
fn acquire(env: &mut Env<'_>) -> Outcome {
    match section(env) {
        Ok(section) => enter_cs(env, section),
        Err(error) => Outcome::Fail(error),
    }
}

#[win32(ntdll, name = "RtlReleasePebLock", level = implement)]
fn release(env: &mut Env<'_>) -> Outcome {
    match section(env) {
        Ok(section) => leave_cs(env, section),
        Err(error) => Outcome::Fail(error),
    }
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(acquire_ABI);
    registry.register(release_ABI);
}
