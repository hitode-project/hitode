//! Process creation failure for the runtime personality until guest processes exist.
use adv_abi::{AnsiStr, GuestPtr, Registry, types::SECURITY_ATTRIBUTES, win32};
use adv_machine::Env;

pub fn register(registry: &mut Registry) {
    registry.register(create_process_ABI);
}

#[win32(kernel32, name = "CreateProcessA", level = safe_fail)]
fn create_process(
    env: &mut Env<'_>,
    _application: AnsiStr,
    _command: AnsiStr,
    _process_security: GuestPtr<SECURITY_ATTRIBUTES>,
    _thread_security: GuestPtr<SECURITY_ATTRIBUTES>,
    _inherit: i32,
    _flags: u32,
    _environment: GuestPtr<()>,
    _directory: AnsiStr,
    _startup: GuestPtr<()>,
    _process_info: GuestPtr<()>,
) -> i32 {
    crate::util::set_last_error(env, 120);
    0
}
