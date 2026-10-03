//! kernel32.dll.

mod atom;
mod environment;
mod file;
mod file_mapping;
pub use file::open as open_file;
mod memory;
mod module;
pub(crate) use module::{find_resource_data, int_or_str, res_id};
mod nlsapi;
mod osversion;
mod process;
mod string;
mod sync;
mod time;
mod toolhelp;
pub use time::filetime_now;

use adv_machine::Registry;

pub(crate) const K32: &str = "kernel32.dll";

pub fn register(reg: &mut Registry) {
    atom::register(reg);
    environment::register(reg);
    file::register(reg);
    file_mapping::register(reg);
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
