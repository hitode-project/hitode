//! The NT platform under Wine's Windows userland.
//!
//! Wine's PE modules, from `ntdll.dll` up, run unmodified in the guest. Where
//! Wine's `ntdll.so` would serve them on a Unix host, this crate serves them
//! from the Hitode machine: NT system calls, process and thread startup, and
//! the client side of the wineserver protocol (issue #58).
//!
//! The win32k system calls go to Wine's `win32u.so`, which runs as guest
//! code that another crate loads (see [`unixlib`] and [`win32k`]).
#![cfg(unix)]

mod apc;
pub mod atom;
pub mod context;
pub mod counters;
pub mod debug;
pub mod directory;
pub mod dispatch;
pub mod environment;
pub mod exception;
pub mod file;
pub mod identity;
pub mod image;
pub mod information;
pub mod kuser;
pub mod memory;
pub mod native;
pub mod nls;
pub mod object;
pub mod params;
pub mod path;
pub mod process;
pub mod protocol;
pub mod registry;
pub mod section;
pub mod server;
pub mod status;
pub mod sync;
pub mod syscalls;
pub mod system;
pub mod thread;
pub mod timezone;
pub mod token;
pub mod unix;
pub mod unixlib;
pub mod win32k;
