//! kernel32.dll.

mod atom;
pub(crate) mod child_process;
mod environment;
mod exception;
pub(crate) use exception::install as install_exceptions;
pub(crate) use exception::uncaught_query_blocker;
mod file;
pub(crate) mod file_mapping;
mod handle;
pub use file::open as open_file;
pub(crate) use file::{open_with_share, read_coherent};
pub(crate) use file::create_directory_path;
pub(crate) use file_mapping::prune as prune_file_mappings;
pub use handle::ProcessHandle;
mod memory;
pub(crate) use memory::protect_memory;
pub(crate) use memory::{allocate_memory, allocation_extent, free_memory, memory_span};
mod module;
pub(crate) use module::{exit_process, exit_thread, install_startup, spawn_thread};
pub(crate) use module::{find_resource_data, int_or_str, res_id};
mod nlsapi;
mod osversion;
mod process;
pub(crate) use environment::entries as environment_entries;
pub(crate) use process::{command_line, configure_codepage};
pub(crate) use process::install_parameters;
mod string;
mod sync;
mod time;
mod timer;
mod toolhelp;
pub use time::filetime_now;
pub(crate) use time::{civil_from_days, days_from_civil};

use adv_abi::Registry;

pub(crate) const K32: &str = "kernel32.dll";

pub fn register(reg: &mut Registry) {
    atom::register(reg);
    child_process::register(reg);
    environment::register(reg);
    exception::register(reg);
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
    timer::register(reg);
    toolhelp::register(reg);
}
