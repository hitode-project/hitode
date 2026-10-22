//! The NT platform under Wine's Windows userland.
//!
//! Wine's PE modules, from `ntdll.dll` up, run unmodified in the guest. Where
//! Wine's `ntdll.so` would serve them on a Unix host, this crate serves them
//! from the Hitode machine: NT system calls, process and thread startup, and
//! the client side of the wineserver protocol (issue #58).
//!
//! So far it holds the server client.
#![cfg(unix)]

pub mod protocol;
pub mod server;
