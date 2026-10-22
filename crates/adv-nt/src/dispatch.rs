//! The two entries through which ntdll.dll leaves the guest.
//!
//! Wine's ntdll.dll calls into its Unix side, ntdll.so, through function
//! pointers that ntdll.so stores in it at startup: one for system calls and
//! one for Unix library calls. Here both are Hitode thunks, so a system call
//! is a single host call with no Unix side in between.

use std::sync::LazyLock;

use adv_cpu::backend::Reg;
use adv_machine::{Conv, Env, HostFn, Outcome, Resolver};

use crate::syscalls::SYSCALLS;
use crate::{
    exception, file, information, kuser, memory, nls, object, process, registry, section, server,
    sync, system, thread, token, unix,
};

/// A system call, given its arguments.
type Syscall = fn(&mut Env<'_>, &[u32]) -> Outcome;

const IMPLEMENTED: &[(&str, Syscall)] = &[
    ("NtAllocateVirtualMemory", memory::allocate),
    ("NtClose", object::close),
    ("NtContinue", exception::resume),
    ("NtCreateEvent", sync::create_event),
    ("NtCreateFile", file::create),
    ("NtCreateKey", registry::create),
    ("NtCreateSection", section::create),
    ("NtCreateThreadEx", thread::create),
    ("NtDelayExecution", sync::delay),
    ("NtOpenDirectoryObject", object::open_directory),
    ("NtOpenFile", file::open),
    ("NtOpenKey", registry::open),
    ("NtOpenKeyEx", registry::open_ex),
    ("NtOpenSection", section::open),
    ("NtFreeVirtualMemory", memory::free),
    ("NtFsControlFile", file::control),
    ("NtGetNextThread", information::next_thread),
    ("NtGetNlsSectionPtr", nls::section),
    ("NtInitializeNlsFiles", nls::initialize),
    ("NtMapViewOfSection", section::map_view),
    ("NtProtectVirtualMemory", memory::protect),
    ("NtQueryAttributesFile", file::query_attributes),
    ("NtQueueApcThread", sync::queue_apc),
    ("NtQueryDefaultLocale", nls::default_locale),
    ("NtQueryInformationProcess", information::query_process),
    ("NtQueryInformationThread", information::query_thread),
    ("NtQueryInformationToken", token::query),
    ("NtQuerySection", section::query),
    ("NtQuerySystemInformation", system::query),
    ("NtQuerySystemTime", system::query_time),
    ("NtQueryValueKey", registry::query_value),
    ("NtQueryVirtualMemory", memory::query),
    ("NtRaiseException", exception::raise),
    ("NtResetEvent", sync::reset_event),
    ("NtResumeThread", thread::resume),
    ("NtSetEvent", sync::set_event),
    ("NtSetValueKey", registry::set_value),
    ("NtTerminateProcess", process::terminate),
    ("NtTerminateThread", thread::terminate),
    ("NtUnmapViewOfSection", section::unmap_view),
    ("NtWaitForMultipleObjects", sync::wait_multiple),
    ("NtWaitForSingleObject", sync::wait_single),
    ("NtWriteFile", file::write),
    (
        "NtWow64IsProcessorFeaturePresent",
        kuser::is_processor_feature_present,
    ),
    ("NtQueryVolumeInformationFile", file::query_volume),
];

/// The implementation of each system call, by number.
static TABLE: LazyLock<[Option<Syscall>; SYSCALLS.len()]> = LazyLock::new(|| {
    let mut table = [None; SYSCALLS.len()];
    for &(name, call) in IMPLEMENTED {
        let number = SYSCALLS
            .iter()
            .position(|&(known, _)| known == name)
            .unwrap_or_else(|| panic!("{name} is not a system call"));
        table[number] = Some(call);
    }
    table
});

/// No system call takes more arguments.
const MAX_ARGUMENTS: usize = 16;

/// The module the dispatchers are registered under; it stands in for
/// ntdll.so.
pub const MODULE: &str = "ntdll.so";
pub const SYSCALL: &str = "__wine_syscall_dispatcher";
pub const UNIX_CALL: &str = "__wine_unix_call_dispatcher";

/// The host functions of the NT platform.
pub struct Platform;

impl Resolver for Platform {
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        if module != MODULE {
            return None;
        }
        let (conv, handler): (_, fn(&mut Env<'_>) -> Outcome) = match name {
            // The system call stub pops its own arguments.
            SYSCALL => (Conv::Cdecl { argc: 0 }, syscall),
            // (handle: u64, code: u32, params: *mut c_void)
            UNIX_CALL => (Conv::Stdcall { argc: 4 }, unix_call),
            _ => return None,
        };
        Some(HostFn {
            leaf: None,
            env_leaf: None,
            conv,
            handler,
            data: 0,
            trace: false,
            guest: None,
        })
    }

    fn provides(&self, module: &str) -> bool {
        module == MODULE
    }
}

/// Entered from a stub in ntdll.dll with the system call number in EAX, the
/// stub's return address at `[esp]` and its caller's frame above it, so
/// argument `i` is `env.arg(i + 1)`.
fn syscall(env: &mut Env<'_>) -> Outcome {
    let number = env.cpu.reg(Reg::Eax) as usize;
    let Some(&(name, bytes)) = SYSCALLS.get(number) else {
        return Outcome::Fail(format!("unknown system call {number:#x}"));
    };
    let Some(call) = TABLE[number] else {
        return Outcome::Fail(format!("system call {name} is not implemented"));
    };
    kuser::refresh(env);
    server::set_time(env);
    let mut args = [0; MAX_ARGUMENTS];
    let count = bytes as usize / 4;
    for (index, arg) in args[..count].iter_mut().enumerate() {
        *arg = env.arg(index as u32 + 1);
    }
    call(env, &args[..count])
}

fn unix_call(env: &mut Env<'_>) -> Outcome {
    let (code, args) = (env.arg(2), env.arg(3));
    unix::call(env, code, args)
}
