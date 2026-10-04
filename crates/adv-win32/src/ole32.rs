//! COM library state and activation of installed in-process personalities.
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
#[win32(ole32, name = "CoCreateInstance", level = implement)]
fn create_instance(
    env: &mut Env<'_>,
    class: GuestPtr<()>,
    outer: GuestPtr<()>,
    context: u32,
    interface: GuestPtr<()>,
    output: GuestPtr<u32>,
) -> Outcome {
    use adv_cpu::backend::GuestExt;
    if output.is_null() {
        return Outcome::Return(0x8000_4003);
    }
    if let Err(error) = env.cpu.write_u32(output.addr(), 0) {
        return Outcome::Fail(error.to_string());
    }
    let mut clsid = [0; 16];
    let mut iid = [0; 16];
    if let Err(error) = env.cpu.read(class.addr(), &mut clsid) {
        return Outcome::Fail(error.to_string());
    }
    if let Err(error) = env.cpu.read(interface.addr(), &mut iid) {
        return Outcome::Fail(error.to_string());
    }
    let factory = env
        .ext
        .try_get::<adv_os::com::ClassFactories>()
        .and_then(|classes| classes.get(&clsid));
    match factory.filter(|_| context & 1 != 0) {
        Some(factory) => factory(env, iid, outer.addr(), output.addr()),
        None => {
            adv_abi::warn_unavailable(
                env,
                "ole32.dll!CoCreateInstance",
                &format!(
                    "no installed class factory for CLSID bytes {clsid:02x?}, IID bytes {iid:02x?}, context {context:#x}"
                ),
            );
            Outcome::Return(0x8004_0154)
        }
    }
}
