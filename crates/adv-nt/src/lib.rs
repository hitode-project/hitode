//! The NT platform under Wine's Windows userland.
//!
//! Wine's PE modules, from `ntdll.dll` up, run unmodified in the guest. Where
//! Wine's `ntdll.so` would serve them on a Unix host, this crate serves them
//! from the Hitode machine: NT system calls, process and thread startup, and
//! the client side of the wineserver protocol (issue #58).
//!
//! So far it starts ntdll.dll up to its first system call, and holds the
//! server client.
#![cfg(unix)]

pub mod context;
pub mod dispatch;
pub mod image;
pub mod memory;
pub mod params;
pub mod process;
pub mod protocol;
pub mod server;
pub mod status;
pub mod syscalls;
