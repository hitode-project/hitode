//! NT allocation marshals owned values to the shared local/remote VM owner.
use crate::kernel32::{allocate_memory, child_process::process_target, free_memory, memory_span};
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
use adv_os::processes;

fn status(error: processes::Error) -> u32 {
    match error.code {
        5 => 0xc0000022,
        6 => 0xc0000008,
        8 => 0xc0000017,
        998 => 0xc0000005,
        487 => 0xc0000018,
        50 => 0xc0000002,
        _ => 0xc000000d,
    }
}
fn outputs(env: &Env<'_>, base: u32, size: u32) -> Result<(), u32> {
    for address in [base, size] {
        check_range(env.cpu, address, 4, Prot::RW).map_err(|_| 0xc0000005u32)?;
    }
    Ok(())
}

#[win32(ntdll, name = "NtAllocateVirtualMemory", level = implement)]
fn allocate(
    env: &mut Env<'_>,
    process: Handle,
    base: GuestPtr<u32>,
    zero_bits: u32,
    size: GuestPtr<u32>,
    flags: u32,
    protection: u32,
) -> Outcome {
    let (base, size) = (base.addr(), size.addr());
    let prepared = (|| -> Result<_, u32> {
        let target = process_target(env, process.0, 8).map_err(status)?;
        outputs(env, base, size)?;
        if zero_bits != 0 {
            warn_unavailable(
                env,
                "ntdll.dll!NtAllocateVirtualMemory",
                "ZeroBits allocation constraints are unavailable",
            );
            return Err(0xc0000002);
        }
        if protection & 0x700 != 0 {
            warn_unavailable(
                env,
                "ntdll.dll!NtAllocateVirtualMemory",
                "page protection modifiers are unavailable",
            );
            return Err(0xc0000002);
        }
        if !matches!(protection, 1 | 2 | 4 | 0x10 | 0x20 | 0x40) {
            return Err(0xc0000045);
        }
        if flags & !(0x1000 | 0x2000 | 0x100000) != 0 {
            warn_unavailable(
                env,
                "ntdll.dll!NtAllocateVirtualMemory",
                "allocation flags outside reserve/commit/top-down are unavailable",
            );
            return Err(0xc0000002);
        }
        if flags & 0x3000 == 0 {
            return Err(0xc000000d);
        }
        let address = env.cpu.read_u32(base).map_err(|_| 0xc0000005u32)?;
        let count = env.cpu.read_u32(size).map_err(|_| 0xc0000005u32)?;
        let (page, length) = memory_span(address, count).map_err(status)?;
        Ok((target, address, count, page, length))
    })();
    let (target, address, count, page, length) = match prepared {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    let pid = target.pid;
    processes::visit(
        env,
        pid,
        None,
        move |remote| allocate_memory(remote, address, count, flags, protection),
        move |caller, result| {
            let allocated = match result {
                Ok(value) => value,
                Err(error) => return Outcome::Return(status(error)),
            };
            let length = if address == 0 {
                length
            } else {
                page + length - allocated
            };
            if outputs(caller, base, size).is_err() {
                // Undo only a new reservation, never release an existing commit's owner.
                if address == 0 || flags & 0x2000 != 0 {
                    return processes::visit(
                        caller,
                        pid,
                        None,
                        move |remote| free_memory(remote, allocated, 0, 0x8000),
                        |caller, cleanup| {
                            if let Err(error) = cleanup {
                                warn_unavailable(
                                    caller,
                                    "ntdll.dll!NtAllocateVirtualMemory",
                                    &format!(
                                        "allocation publication rollback failed: {}",
                                        error.message
                                    ),
                                );
                            }
                            Outcome::Return(0xc0000005)
                        },
                    );
                }
                return Outcome::Return(0xc0000005);
            }
            if caller.cpu.write_u32(base, allocated).is_err()
                || caller.cpu.write_u32(size, length).is_err()
            {
                return Outcome::Return(0xc0000005);
            }
            Outcome::Return(0)
        },
    )
}

#[win32(ntdll, name = "NtFreeVirtualMemory", level = implement)]
fn free(
    env: &mut Env<'_>,
    process: Handle,
    base: GuestPtr<u32>,
    size: GuestPtr<u32>,
    flags: u32,
) -> Outcome {
    let (process, base, size) = (process.0, base.addr(), size.addr());
    let prepared = (|| -> Result<_, u32> {
        let target = process_target(env, process, 8).map_err(status)?;
        outputs(env, base, size)?;
        if !matches!(flags, 0x4000 | 0x8000) {
            return Err(0xc00000f2); // STATUS_INVALID_PARAMETER_4
        }
        let address = env.cpu.read_u32(base).map_err(|_| 0xc0000005u32)?;
        let count = env.cpu.read_u32(size).map_err(|_| 0xc0000005u32)?;
        Ok((target, address, count))
    })();
    let (target, address, count) = match prepared {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    processes::visit(
        env,
        target.pid,
        None,
        move |remote| Ok(free_owned(remote, address, count, flags)),
        move |caller, result| {
            let (page, length) = match result {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => return Outcome::Return(error),
                Err(error) if error.code == 87 => return Outcome::Return(0xc000010a),
                Err(error) => return Outcome::Return(status(error)),
            };
            // A released reservation cannot be resurrected if caller outputs
            // became inaccessible while the target operation was serviced.
            if outputs(caller, base, size).is_err()
                || caller.cpu.write_u32(base, page).is_err()
                || caller.cpu.write_u32(size, length).is_err()
            {
                return Outcome::Return(0xc0000005);
            }
            Outcome::Return(0)
        },
    )
}

fn free_owned(env: &mut Env<'_>, address: u32, count: u32, flags: u32) -> Result<(u32, u32), u32> {
    let (owner, extent) = crate::kernel32::allocation_extent(env, address).ok_or(0xc00000a0u32)?; // STATUS_MEMORY_NOT_ALLOCATED
    let (page, length) = if count == 0 {
        let page = address & !4095;
        if page != owner {
            return Err(0xc000009f); // STATUS_FREE_VM_NOT_AT_BASE
        }
        (page, extent)
    } else {
        memory_span(address, count).map_err(status)?
    };
    if flags == 0x8000 && count != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!NtFreeVirtualMemory",
            "partial reservation release is not modeled",
        );
        return Err(0xc0000002);
    }
    if u64::from(page) + u64::from(length) > u64::from(owner) + u64::from(extent) {
        return Err(0xc000001a); // STATUS_UNABLE_TO_FREE_VM
    }
    free_memory(env, page, if count == 0 { 0 } else { length }, flags).map_err(status)?;
    Ok((page, length))
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(allocate_ABI);
    registry.register(adv_abi::Api {
        name: "ZwAllocateVirtualMemory",
        ..allocate_ABI
    });
    registry.register(free_ABI);
    registry.register(adv_abi::Api {
        name: "ZwFreeVirtualMemory",
        ..free_ABI
    });
}

#[cfg(test)]
mod tests;
