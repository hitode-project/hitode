//! x86 Automation VARIANT operations over shared BSTR ownership and real guest
//! IUnknown vtables. No duplicate string registry or COM reference counter.
use adv_abi::{Api, GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
use adv_os::com::{MAX_BSTR_BYTES, copy_bstr, free_bstr, owns_bstr};

const INVALID: u32 = 0x80070057;
const BAD_TYPE: u32 = 0x80020008;
const NOT_IMPLEMENTED: u32 = 0x80004001;
const OUT_OF_MEMORY: u32 = 0x8007000e;
type Variant = [u8; 16];
type Then = Box<dyn FnOnce(&mut Env<'_>) -> Outcome>;

pub(super) fn register(registry: &mut Registry) {
    for (api, ordinal) in [(init_ABI, "#8"), (clear_ABI, "#9"), (copy_ABI, "#10")] {
        registry.register(api);
        registry.register(Api {
            name: ordinal,
            ..api
        });
    }
}

fn warn(env: &mut Env<'_>, api: &'static str, reason: &str, result: u32) -> u32 {
    adv_abi::warn_unavailable(env, api, reason);
    result
}
fn read(env: &Env<'_>, pointer: u32, writable: bool) -> Result<Variant, u32> {
    check_range(
        env.cpu,
        pointer,
        16,
        if writable { Prot::RW } else { Prot::READ },
    )
    .map_err(|_| INVALID)?;
    let mut bytes = [0; 16];
    env.cpu.read(pointer, &mut bytes).map_err(|_| INVALID)?;
    Ok(bytes)
}
fn vt(bytes: &Variant) -> u16 {
    u16::from_le_bytes(bytes[..2].try_into().unwrap())
}
fn pointer(bytes: &Variant) -> u32 {
    u32::from_le_bytes(bytes[8..12].try_into().unwrap())
}

fn supported(env: &mut Env<'_>, kind: u16, api: &'static str) -> Result<(), u32> {
    let base = kind & 0x0fff;
    if (base > 23 || base == 15) && base != 36
        || kind & 0x9000 != 0
        || (kind & 0x4000 != 0 && base <= 1)
    {
        return Err(BAD_TYPE);
    }
    if kind & 0x2000 != 0 || base == 36 {
        return Err(warn(
            env,
            api,
            "SAFEARRAY/record ownership is not implemented; the variant is retained",
            NOT_IMPLEMENTED,
        ));
    }
    Ok(())
}
fn method(env: &Env<'_>, this: u32, slot: u32) -> Result<u32, u32> {
    check_range(env.cpu, this, 4, Prot::READ).map_err(|_| INVALID)?;
    let table = env.cpu.read_u32(this).map_err(|_| INVALID)?;
    let address = table.checked_add(slot * 4).ok_or(INVALID)?;
    check_range(env.cpu, address, 4, Prot::READ).map_err(|_| INVALID)?;
    let target = env.cpu.read_u32(address).map_err(|_| INVALID)?;
    check_range(env.cpu, target, 1, Prot::EXEC).map_err(|_| INVALID)?;
    Ok(target)
}
fn empty(env: &mut Env<'_>, address: u32) -> Result<(), u32> {
    // Native VariantClear only replaces vt, preserving reserved/payload bytes.
    check_range(env.cpu, address, 16, Prot::WRITE).map_err(|_| INVALID)?;
    env.cpu.write(address, &[0, 0]).map_err(|_| INVALID)
}

fn clear_then(env: &mut Env<'_>, address: u32, api: &'static str, then: Then) -> Outcome {
    let bytes = match read(env, address, true) {
        Ok(b) => b,
        Err(e) => return Outcome::Return(e),
    };
    let kind = vt(&bytes);
    if let Err(e) = supported(env, kind, api) {
        return Outcome::Return(e);
    }
    if kind & 0x4000 != 0 {
        // References borrow their pointee; never free/release it.
        return match empty(env, address) {
            Ok(()) => then(env),
            Err(e) => Outcome::Return(e),
        };
    }
    let value = pointer(&bytes);
    if kind == 8 && value != 0 {
        if !owns_bstr(env, value) {
            return Outcome::Return(warn(
                env,
                api,
                "BSTR is not owned by the shared Automation allocator",
                INVALID,
            ));
        }
        free_bstr(env, value);
    }
    if matches!(kind, 9 | 13) && value != 0 {
        let target = match method(env, value, 2) {
            Ok(t) => t,
            Err(e) => return Outcome::Return(e),
        };
        return Outcome::call(target, [value], move |env, _| match empty(env, address) {
            Ok(()) => then(env),
            Err(e) => Outcome::Return(e),
        });
    }
    match empty(env, address) {
        Ok(()) => then(env),
        Err(e) => Outcome::Return(e),
    }
}

#[win32(oleaut32, name = "VariantInit", level = implement)]
fn init(env: &mut Env<'_>, variant: GuestPtr<Variant>) -> Outcome {
    match check_range(env.cpu, variant.addr(), 16, Prot::WRITE)
        .and_then(|_| env.cpu.write(variant.addr(), &[0; 16]))
    {
        Ok(()) => Outcome::Return(0),
        Err(_) => {
            warn(
                env,
                "oleaut32.dll!VariantInit",
                "VARIANT output is not a writable 16-byte guest value",
                INVALID,
            );
            Outcome::Fail("VariantInit: invalid guest VARIANT output".into())
        }
    }
}
#[win32(oleaut32, name = "VariantClear", level = implement)]
fn clear(env: &mut Env<'_>, variant: GuestPtr<Variant>) -> Outcome {
    clear_then(
        env,
        variant.addr(),
        "oleaut32.dll!VariantClear",
        Box::new(|_| Outcome::Return(0)),
    )
}
#[win32(oleaut32, name = "VariantCopy", level = implement)]
fn copy(env: &mut Env<'_>, destination: GuestPtr<Variant>, source: GuestPtr<Variant>) -> Outcome {
    let bytes = match read(env, source.addr(), false) {
        Ok(b) => b,
        Err(e) => return Outcome::Return(e),
    };
    if let Err(e) = supported(env, vt(&bytes), "oleaut32.dll!VariantCopy") {
        return Outcome::Return(e);
    }
    if read(env, destination.addr(), true).is_err() {
        return Outcome::Return(INVALID);
    }
    if destination.addr() == source.addr() {
        return Outcome::Return(0);
    }
    clear_then(
        env,
        destination.addr(),
        "oleaut32.dll!VariantCopy",
        Box::new(move |env| {
            // Destination Release may have executed guest code. Re-read the source
            // rather than copying a stale snapshot after that callback returns.
            let mut bytes = match read(env, source.addr(), false) {
                Ok(b) => b,
                Err(e) => return Outcome::Return(e),
            };
            let kind = vt(&bytes);
            if let Err(e) = supported(env, kind, "oleaut32.dll!VariantCopy") {
                return Outcome::Return(e);
            }
            if check_range(env.cpu, destination.addr(), 16, Prot::WRITE).is_err() {
                return Outcome::Return(INVALID);
            }
            let value = pointer(&bytes);
            let mut allocated = 0;
            if kind == 8 && value != 0 {
                if !owns_bstr(env, value) {
                    return Outcome::Return(warn(
                        env,
                        "oleaut32.dll!VariantCopy",
                        "source BSTR is not owned by the shared Automation allocator",
                        INVALID,
                    ));
                }
                let length = match value.checked_sub(4).and_then(|prefix| {
                    check_range(env.cpu, prefix, 4, Prot::READ).ok()?;
                    env.cpu.read_u32(prefix).ok()
                }) {
                    Some(n) if n <= MAX_BSTR_BYTES => n,
                    _ => return Outcome::Return(INVALID),
                };
                if check_range(env.cpu, value, length, Prot::READ).is_err() {
                    return Outcome::Return(INVALID);
                }
                allocated = copy_bstr(env, value, length);
                if allocated == 0 {
                    return Outcome::Return(OUT_OF_MEMORY);
                }
                bytes[8..12].copy_from_slice(&allocated.to_le_bytes());
            }
            let addref = if matches!(kind, 9 | 13) && value != 0 {
                match method(env, value, 1) {
                    Ok(t) => Some(t),
                    Err(e) => return Outcome::Return(e),
                }
            } else {
                None
            };
            if env.cpu.write(destination.addr(), &bytes).is_err() {
                if allocated != 0 {
                    free_bstr(env, allocated);
                }
                return Outcome::Return(INVALID);
            }
            // Publishing before AddRef matches native callback observability. BYREF
            // interfaces only copy the reference address and never invoke this path.
            if let Some(target) = addref {
                Outcome::call(target, [value], |_, _| Outcome::Return(0))
            } else {
                Outcome::Return(0)
            }
        }),
    )
}
