//! Virtual page locks use the executor's actual backing-page owner.
use adv_abi::{GuestPtr, Registry, warn_unavailable, win32};
use adv_cpu::backend::{PageLockError, Prot};
use adv_machine::Env;

use crate::util::set_last_error;

fn change(env: &mut Env<'_>, address: u32, size: u32, lock: bool) -> i32 {
    let Ok((page, length)) = super::virtual_memory::span(address, size) else {
        set_last_error(env, 87);
        return 0;
    };
    if lock
        && (!env.cpu.accessible(page, length, Prot::default())
            || (0..length / 4096).any(|i| {
                let at = page + i * 4096;
                ![Prot::READ, Prot::WRITE, Prot::EXEC]
                    .into_iter()
                    .any(|access| env.cpu.accessible(at, 4096, access))
            }))
    {
        set_last_error(env, 998);
        return 0;
    }
    let result = if lock {
        env.cpu.lock_pages(page, length)
    } else {
        env.cpu.unlock_pages(page, length)
    };
    match result {
        Ok(()) => 1,
        Err(error) => {
            let error = match error {
                PageLockError::Win32(error) => error,
                PageLockError::Unsupported => {
                    warn_unavailable(
                        env,
                        if lock {
                            "kernel32.dll!VirtualLock"
                        } else {
                            "kernel32.dll!VirtualUnlock"
                        },
                        "physical pinning is available only for ordinary private Windows executor pages; this backend/backing has no supported pin owner",
                    );
                    50 // ERROR_NOT_SUPPORTED
                }
            };
            set_last_error(env, error);
            0
        }
    }
}

#[win32(kernel32, name = "VirtualLock", level = implement)]
fn lock(env: &mut Env<'_>, address: GuestPtr<()>, size: u32) -> i32 {
    change(env, address.addr(), size, true)
}

#[win32(kernel32, name = "VirtualUnlock", level = implement)]
fn unlock(env: &mut Env<'_>, address: GuestPtr<()>, size: u32) -> i32 {
    change(env, address.addr(), size, false)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(lock_ABI);
    registry.register(unlock_ABI);
}
