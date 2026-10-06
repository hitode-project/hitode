//! Property sheets have no frame/page backend yet. Expose callable failures,
//! without allocating page identities or pretending that a user cancelled UI.
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;

use crate::util::set_last_error;

const ERROR_NOACCESS: u32 = 998;
const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
const PSH_MODELESS: u32 = 0x400;

fn structure(env: &Env<'_>, address: u32, sizes: &[u32]) -> Result<u32, u32> {
    if address == 0 {
        return Err(87);
    }
    check_range(env.cpu, address, 4, Prot::READ).map_err(|_| ERROR_NOACCESS)?;
    let size = env.cpu.read_u32(address).map_err(|_| ERROR_NOACCESS)?;
    if !sizes.contains(&size) {
        return Err(87);
    }
    check_range(env.cpu, address, size, Prot::READ).map_err(|_| ERROR_NOACCESS)?;
    env.cpu.read_u32(address + 4).map_err(|_| ERROR_NOACCESS)
}

#[win32(comctl32, name = "CreatePropertySheetPageA", level = safe_fail)]
fn create_page(env: &mut Env<'_>, page: GuestPtr<u8>) -> Handle {
    // SDK prsht.h's four x86 PROPSHEETPAGEA revisions are 40/48/52/56 bytes.
    let error = match structure(env, page.addr(), &[40, 48, 52, 56]) {
        Err(error) => error,
        Ok(_) => {
            adv_abi::warn_unavailable(
                env,
                "comctl32.dll!CreatePropertySheetPageA",
                "property sheet page ownership and callbacks are not implemented",
            );
            ERROR_CALL_NOT_IMPLEMENTED
        }
    };
    set_last_error(env, error);
    Handle(0)
}

#[win32(comctl32, name = "PropertySheetA", level = safe_fail)]
fn property_sheet(env: &mut Env<'_>, header: GuestPtr<u8>) -> i32 {
    // SDK x86 PROPSHEETHEADERA V1/V2; no guest callback is invoked on failure.
    let flags = match structure(env, header.addr(), &[40, 52]) {
        Err(error) => {
            set_last_error(env, error);
            return -1;
        }
        Ok(flags) => flags,
    };
    // The documented >99-page rejection has no extended-error diagnosis.
    if env.cpu.read_u32(header.addr() + 24).unwrap_or(0) > 99 {
        set_last_error(env, 0);
        return -1;
    }
    adv_abi::warn_unavailable(
        env,
        "comctl32.dll!PropertySheetA",
        "property sheet frame, page activation and modal/modeless lifecycle are not implemented",
    );
    set_last_error(env, ERROR_CALL_NOT_IMPLEMENTED);
    if flags & PSH_MODELESS != 0 { 0 } else { -1 }
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_page_ABI);
    registry.register(property_sheet_ABI);
}
