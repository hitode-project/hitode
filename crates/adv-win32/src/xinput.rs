//! XInput for the shared input profile with no configured game controllers.
use adv_abi::{GuestPtr, Registry, win32};
use adv_machine::Env;

const ERROR_BAD_ARGUMENTS: u32 = 160;
const ERROR_DEVICE_NOT_CONNECTED: u32 = 1167;

pub fn register(registry: &mut Registry) {
    for api in [
        get_state_9_ABI,
        set_state_9_ABI,
        get_capabilities_9_ABI,
        get_state_13_ABI,
        set_state_13_ABI,
        get_capabilities_13_ABI,
    ] {
        registry.register(api);
    }
}

/// Native checks the slot and NULL pointer before looking for a device. With
/// no device it never accesses a non-NULL state/vibration/capabilities pointer.
/// Error codes are returned directly; the SDK does not use SetLastError.
fn disconnected(index: u32, flags: u32, pointer: GuestPtr<()>) -> u32 {
    if index >= 4 || flags > 1 || pointer.is_null() {
        ERROR_BAD_ARGUMENTS
    } else {
        ERROR_DEVICE_NOT_CONNECTED
    }
}

#[win32(xinput9_1_0, name = "XInputGetState", level = safe_fail)]
fn get_state_9(_env: &mut Env<'_>, index: u32, output: GuestPtr<()>) -> u32 {
    disconnected(index, 0, output)
}

#[win32(xinput9_1_0, name = "XInputSetState", level = safe_fail)]
fn set_state_9(_env: &mut Env<'_>, index: u32, vibration: GuestPtr<()>) -> u32 {
    disconnected(index, 0, vibration)
}

#[win32(xinput9_1_0, name = "XInputGetCapabilities", level = safe_fail)]
fn get_capabilities_9(
    _env: &mut Env<'_>,
    index: u32,
    flags: u32,
    output: GuestPtr<()>,
) -> u32 {
    // The legacy fixed capabilities apply to connected devices, not a
    // fabricated controller. The absent-device native result is still 1167.
    disconnected(index, flags, output)
}

#[win32(xinput1_3, name = "XInputGetState", level = safe_fail)]
fn get_state_13(_env: &mut Env<'_>, index: u32, output: GuestPtr<()>) -> u32 {
    disconnected(index, 0, output)
}

#[win32(xinput1_3, name = "XInputSetState", level = safe_fail)]
fn set_state_13(_env: &mut Env<'_>, index: u32, vibration: GuestPtr<()>) -> u32 {
    disconnected(index, 0, vibration)
}

#[win32(xinput1_3, name = "XInputGetCapabilities", level = safe_fail)]
fn get_capabilities_13(
    _env: &mut Env<'_>,
    index: u32,
    flags: u32,
    output: GuestPtr<()>,
) -> u32 {
    disconnected(index, flags, output)
}
