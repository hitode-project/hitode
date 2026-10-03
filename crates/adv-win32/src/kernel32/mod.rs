//! kernel32.dll.

mod atom;
mod environment;
mod file;
mod file_mapping;
mod handle;
pub use file::open as open_file;
pub use handle::ProcessHandle;
mod memory;
mod module;
pub(crate) use module::{find_resource_data, int_or_str, res_id};
pub(crate) use module::{install_startup, spawn_thread};
mod nlsapi;
mod osversion;
mod process;
mod string;
mod sync;
mod time;
mod toolhelp;
pub use time::filetime_now;

use adv_abi::Registry;

pub(crate) const K32: &str = "kernel32.dll";

pub fn register(reg: &mut Registry) {
    atom::register(reg);
    environment::register(reg);
    file::register(reg);
    file_mapping::register(reg);
    handle::register(reg);
    memory::register(reg);
    module::register(reg);
    nlsapi::register(reg);
    osversion::register(reg);
    process::register(reg);
    string::register(reg);
    sync::register(reg);
    time::register(reg);
    toolhelp::register(reg);
}
pub mod unsupported_process;
