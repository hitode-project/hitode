//! SAFEARRAY descriptors and elements share guest memory and process heap ownership.
//! API contract: https://learn.microsoft.com/windows/win32/api/oleauto/nf-oleauto-safearrayaccessdata
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;
mod owned;
pub(super) use owned::{clone_then, destroy_then};

#[cfg(test)]
mod tests;

const S_OK: i32 = 0;
const E_UNEXPECTED: i32 = 0x8000_ffff_u32 as i32;
const E_INVALIDARG: i32 = 0x8007_0057_u32 as i32;
const LOCKS: u32 = 8;
const DATA: u32 = 12;
/// Shared lock-counter limit for direct locks and data access.
const MAX_LOCKS: u32 = 0xffff;

pub(super) fn register(registry: &mut Registry) {
    owned::register(registry);
    for api in [lock_ABI, unlock_ABI, access_ABI, unaccess_ABI] {
        registry.register(api);
    }
}

fn adjust(env: &mut Env<'_>, array: u32, delta: i32) -> i32 {
    if array == 0 {
        return E_INVALIDARG;
    }
    if check_range(env.cpu, array, 16, Prot::READ | Prot::WRITE).is_err() {
        return E_INVALIDARG;
    }
    let Ok(locks) = env.cpu.read_u32(array + LOCKS) else {
        return E_INVALIDARG;
    };
    let next = match delta {
        1 if locks < MAX_LOCKS => locks + 1,
        -1 if locks > 0 => locks - 1,
        _ => return E_UNEXPECTED,
    };
    if env.cpu.write_u32(array + LOCKS, next).is_err() {
        return E_INVALIDARG;
    }
    S_OK
}

#[win32(oleaut32, name = "SafeArrayLock", level = implement)]
fn lock(env: &mut Env<'_>, array: GuestPtr<()>) -> i32 {
    adjust(env, array.addr(), 1)
}
#[win32(oleaut32, name = "SafeArrayUnlock", level = implement)]
fn unlock(env: &mut Env<'_>, array: GuestPtr<()>) -> i32 {
    adjust(env, array.addr(), -1)
}
#[win32(oleaut32, name = "SafeArrayAccessData", level = implement)]
fn access(env: &mut Env<'_>, array: GuestPtr<()>, output: GuestPtr<u32>) -> i32 {
    if array.addr() == 0 || check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return E_INVALIDARG;
    }
    let result = adjust(env, array.addr(), 1);
    let data = if result == S_OK {
        env.cpu.read_u32(array.addr() + DATA).unwrap_or(0)
    } else {
        0
    };
    let _ = output.write(env.cpu, data);
    result
}
#[win32(oleaut32, name = "SafeArrayUnaccessData", level = implement)]
fn unaccess(env: &mut Env<'_>, array: GuestPtr<()>) -> i32 {
    adjust(env, array.addr(), -1)
}
