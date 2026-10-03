//! COM library state and unavailable-class failure contracts.
use adv_abi::{GuestPtr, Registry, win32};
use adv_machine::{Env, Outcome};

pub fn register(registry: &mut Registry) {
    for api in [
        initialize_ABI,
        initialize_ex_ABI,
        uninitialize_ABI,
        create_instance_ABI,
    ] {
        registry.register(api);
    }
}

#[win32(ole32, name = "CoInitialize", level = state)]
fn initialize(_env: &mut Env<'_>, _reserved: GuestPtr<()>) -> i32 {
    0
}
#[win32(ole32, name = "CoInitializeEx", level = state)]
fn initialize_ex(_env: &mut Env<'_>, _reserved: GuestPtr<()>, _flags: u32) -> i32 {
    0
}
#[win32(ole32, name = "CoUninitialize", level = state)]
fn uninitialize(_env: &mut Env<'_>) {}
#[win32(ole32, name = "CoCreateInstance", level = safe_fail)]
fn create_instance(
    env: &mut Env<'_>,
    _class: GuestPtr<()>,
    _outer: GuestPtr<()>,
    _context: u32,
    _interface: GuestPtr<()>,
    output: GuestPtr<u32>,
) -> Outcome {
    use adv_cpu::backend::GuestExt;
    if let Err(error) = env.cpu.write_u32(output.addr(), 0) {
        return Outcome::Fail(error.to_string());
    }
    Outcome::Return(0x8004_0154)
}
