//! XInput for the shared input profile with no configured game controllers.
use adv_abi::{GuestPtr, Registry, win32};
use adv_machine::Env;

const ERROR_BAD_ARGUMENTS: u32 = 160;
const ERROR_DEVICE_NOT_CONNECTED: u32 = 1167;

pub fn register(registry: &mut Registry) {
    for module in [
        "xinput1_1.dll",
        "xinput1_2.dll",
        "xinput1_3.dll",
        "xinput9_1_0.dll",
    ] {
        for api in [get_state_ABI, set_state_ABI, get_capabilities_ABI] {
            registry.register(adv_abi::Api { module, ..api });
        }
        if module != "xinput9_1_0.dll" {
            registry.register(adv_abi::Api {
                module,
                ..enable_ABI
            });
        }
        if module == "xinput1_3.dll" {
            for api in [battery_ABI, keystroke_ABI] {
                registry.register(adv_abi::Api { module, ..api });
            }
        }
    }
    for (ordinal, target) in [
        ("#2", "XInputGetState"),
        ("#3", "XInputSetState"),
        ("#5", "XInputEnable"),
    ] {
        registry
            .register_alias("xinput1_3.dll", ordinal, target)
            .expect("legacy native ordinals share the selected SDK ABI");
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

#[win32(xinput1_3, name = "XInputGetState", level = safe_fail)]
fn get_state(_env: &mut Env<'_>, index: u32, output: GuestPtr<()>) -> u32 {
    disconnected(index, 0, output)
}

#[win32(xinput1_3, name = "XInputSetState", level = safe_fail)]
fn set_state(_env: &mut Env<'_>, index: u32, vibration: GuestPtr<()>) -> u32 {
    disconnected(index, 0, vibration)
}

#[win32(xinput1_3, name = "XInputGetCapabilities", level = safe_fail)]
fn get_capabilities(_env: &mut Env<'_>, index: u32, flags: u32, output: GuestPtr<()>) -> u32 {
    disconnected(index, flags, output)
}

#[win32(xinput1_3, name = "XInputEnable", level = implement)]
fn enable(_env: &mut Env<'_>, _enabled: i32) {
    // With no configured devices there is no vibration or input stream to mute.
}

#[win32(xinput1_3, name = "XInputGetBatteryInformation", level = safe_fail)]
fn battery(_env: &mut Env<'_>, index: u32, device: u32, output: GuestPtr<()>) -> u32 {
    disconnected(index, u32::from(!matches!(device, 0 | 2)) * 2, output)
}

#[win32(xinput1_3, name = "XInputGetKeystroke", level = safe_fail)]
fn keystroke(_env: &mut Env<'_>, index: u32, reserved: u32, output: GuestPtr<()>) -> u32 {
    let index = if index == 255 { 0 } else { index }; // XUSER_INDEX_ANY
    disconnected(index, u32::from(reserved != 0) * 2, output)
}
