//! COM library state and activation of installed in-process personalities.
use adv_abi::{GuestPtr, Registry, win32};
use adv_machine::{Env, Outcome};

mod runtime;
pub mod tsf;

pub fn register(registry: &mut Registry) {
    tsf::register(registry);
    runtime::register(registry);
    for api in [
        initialize_ABI,
        initialize_ex_ABI,
        uninitialize_ABI,
        create_instance_ABI,
        free_unused_ABI,
    ] {
        registry.register(api);
    }
}

pub fn install<B: adv_cpu::backend::Backend>(machine: &mut adv_machine::Machine<B>) {
    tsf::install(machine);
}

#[win32(ole32, name = "CoInitialize", level = state)]
fn initialize(env: &mut Env<'_>, reserved: GuestPtr<()>) -> u32 {
    runtime::initialize(env, reserved.addr(), 2, false)
}
#[win32(ole32, name = "CoInitializeEx", level = state)]
fn initialize_ex(env: &mut Env<'_>, reserved: GuestPtr<()>, flags: u32) -> u32 {
    runtime::initialize(env, reserved.addr(), flags, false)
}
#[win32(ole32, name = "CoUninitialize", level = state)]
fn uninitialize(env: &mut Env<'_>) {
    runtime::uninitialize(env, false);
}
#[win32(ole32, name = "CoFreeUnusedLibraries", level = state)]
fn free_unused(_env: &mut Env<'_>) {
    // Activation uses installed personality factories. None owns a dynamically
    // loaded guest COM server with a DllCanUnloadNow/unload contract.
}
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
