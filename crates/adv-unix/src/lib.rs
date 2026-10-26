//! Wine's Unix libraries, such as `win32u.so`, as guest ELF code on the NT
//! platform.
//!
//! Wine builds the Unix side of a builtin module as an ELF shared object
//! that links against ntdll.so and the C library. Here the loader maps the
//! object into the guest like the PE images around it. Most of what it
//! imports from ntdll.so binds to ntdll.dll's exports of the same name; the
//! rest of ntdll.so, and the parts of the C library the libraries use, are
//! host functions. FreeType is built into the host with i386 mirrors of
//! the public records win32u reads.
#![cfg(unix)]

pub mod builtin;
pub mod elf;
mod file;
mod freetype;
mod libc;
pub mod loader;
mod ntdll;
mod pthread;
mod stdcall;

use adv_cpu::backend::{Backend, GuestExt};
use adv_machine::{Conv, Env, HostFn, Machine, Outcome, Resolver};

/// A host function of a Unix library's import.
type Function = (Conv, fn(&mut Env<'_>) -> Outcome);

const NTDLL: &str = "ntdll.so";
const LIBC: &str = "libc.so.6";
const LIBM: &str = "libm.so.6";

/// The host functions of the NT platform and of the libraries Wine's Unix
/// libraries link against.
pub struct Platform;

impl Resolver for Platform {
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        if let Some(function) = adv_nt::dispatch::Platform.host_fn(module, name) {
            return Some(function);
        }
        let (conv, handler) = match module {
            NTDLL => ntdll::function(name)?,
            LIBC | LIBM => libc::function(name)
                .or_else(|| file::function(name))
                .or_else(|| pthread::function(name))?,
            freetype::SONAME => freetype::function(name)?,
            _ => return None,
        };
        Some(HostFn {
            leaf: None,
            env_leaf: match (module, name) {
                (LIBC, "pthread_mutex_lock" | "pthread_mutex_unlock")
                | (freetype::SONAME, "FT_Get_Char_Index") => Some(return_leaf),
                _ => None,
            },
            conv,
            handler,
            data: 0,
            trace: false,
            guest: None,
        })
    }

    fn provides(&self, module: &str) -> bool {
        matches!(module, NTDLL | LIBC | LIBM | freetype::SONAME)
    }
}

/// Lets builtin modules load their Unix libraries. `machine` uses
/// [`Platform`].
pub fn install<B: Backend>(machine: &mut Machine<B>) {
    machine
        .ext
        .insert(adv_nt::unixlib::Loader(loader::load_unixlib));
}

// Admitted handlers have no side effects on non-return outcomes.
fn return_leaf(env: &mut Env<'_>) -> Option<u64> {
    let binding = env.sys.thunks.get(env.thunk).f;
    for i in 0..u32::from(binding.conv.argc()) {
        env.cpu.arg(i).ok()?;
    }
    match (binding.handler)(env) {
        Outcome::Return(value) => {
            Some((u64::from(adv_abi::leaf_edx(env)) << 32) | u64::from(value))
        }
        _ => None,
    }
}
