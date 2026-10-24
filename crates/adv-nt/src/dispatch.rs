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
    atom, debug, directory, exception, file, information, kuser, memory, nls, object, process,
    registry, section, server, sync, system, thread, token, unix, unixlib, win32k,
};

/// A system call, given its arguments.
type Syscall = fn(&mut Env<'_>, &[u32]) -> Outcome;

const IMPLEMENTED: &[(&str, Syscall)] = &[
    ("NtAcceptConnectPort", sync::port_stub),
    ("NtAddAtom", atom::add),
    ("NtAlertMultipleThreadByThreadId", sync::alert_multiple),
    ("NtAlertThreadByThreadId", sync::alert_thread),
    (
        "NtAllocateLocallyUniqueId",
        token::allocate_locally_unique_id,
    ),
    ("NtAllocateVirtualMemory", memory::allocate),
    ("NtAllocateVirtualMemoryEx", memory::allocate_ex),
    ("NtApphelpCacheControl", system::apphelp_cache_control),
    ("NtAreMappedFilesTheSame", section::are_same),
    ("NtCallbackReturn", win32k::callback_return),
    ("NtCancelTimer", sync::cancel_timer),
    ("NtClose", object::close),
    ("NtCommitTransaction", sync::commit_transaction),
    ("NtCompleteConnectPort", sync::port_stub),
    ("NtConnectPort", sync::port_stub),
    ("NtContinue", exception::resume),
    (
        "NtConvertBetweenAuxiliaryCounterAndPerformanceCounter",
        sync::convert_counter,
    ),
    ("NtCreateDebugObject", sync::create_debug),
    ("NtCreateDirectoryObject", object::create_directory),
    ("NtCreateEvent", sync::create_event),
    ("NtCreateFile", file::create),
    ("NtCreateIoCompletion", sync::create_completion),
    ("NtCreateJobObject", sync::create_job),
    ("NtCreateKey", registry::create),
    ("NtCreateKeyedEvent", sync::create_keyed_event),
    ("NtCreateMutant", sync::create_mutant),
    ("NtCreateNamedPipeFile", file::create_named_pipe),
    ("NtCreatePagingFile", memory::create_paging_file),
    ("NtCreatePort", sync::port_stub),
    ("NtCreateSection", section::create),
    ("NtCreateSectionEx", section::create_ex),
    ("NtCreateSemaphore", sync::create_semaphore),
    ("NtCreateSymbolicLinkObject", object::create_symlink),
    ("NtCreateThreadEx", thread::create),
    ("NtCreateTimer", sync::create_timer),
    ("NtCreateTransaction", sync::create_transaction),
    ("NtCreateUserProcess", process::create_user_process),
    ("NtDebugActiveProcess", debug::attach),
    ("NtDebugContinue", debug::continue_event),
    ("NtDelayExecution", sync::delay),
    ("NtDeleteAtom", atom::delete),
    ("NtDeleteKey", registry::delete),
    ("NtDeleteValueKey", registry::delete_value),
    ("NtDisplayString", system::display_string),
    ("NtDuplicateObject", object::duplicate),
    ("NtDuplicateToken", token::duplicate),
    ("NtEnumerateKey", registry::enumerate),
    ("NtEnumerateValueKey", registry::enumerate_value),
    ("NtFlushProcessWriteBuffers", memory::flush_write_buffers),
    ("NtFlushVirtualMemory", memory::flush_virtual),
    ("NtGetWriteWatch", memory::get_write_watch),
    ("NtImpersonateClientOfPort", sync::port_stub),
    ("NtInitiatePowerAction", system::initiate_power_action),
    ("NtListenPort", sync::port_stub),
    ("NtLoadDriver", system::load_driver),
    ("NtLockVirtualMemory", memory::lock_virtual),
    (
        "NtMapUserPhysicalPagesScatter",
        memory::map_physical_scatter,
    ),
    ("NtMapViewOfSectionEx", section::map_view_ex),
    ("NtNotifyChangeKey", registry::notify_change),
    (
        "NtNotifyChangeMultipleKeys",
        registry::notify_change_multiple,
    ),
    ("NtOpenDirectoryObject", object::open_directory),
    ("NtOpenEvent", sync::open_event),
    ("NtOpenFile", file::open),
    ("NtOpenIoCompletion", sync::open_completion),
    ("NtOpenJobObject", sync::open_job),
    ("NtOpenKey", registry::open),
    ("NtOpenKeyEx", registry::open_ex),
    ("NtOpenKeyedEvent", sync::open_keyed_event),
    ("NtOpenMutant", sync::open_mutant),
    ("NtOpenProcess", information::open_process),
    ("NtOpenProcessToken", token::open_process),
    ("NtOpenSection", section::open),
    ("NtOpenSemaphore", sync::open_semaphore),
    ("NtOpenSymbolicLinkObject", object::open_symlink),
    ("NtOpenThread", information::open_thread),
    ("NtOpenThreadToken", token::open_thread),
    ("NtOpenTimer", sync::open_timer),
    ("NtFindAtom", atom::find),
    ("NtFreeVirtualMemory", memory::free),
    ("NtFlushInstructionCache", memory::flush_instruction_cache),
    ("NtFlushKey", registry::flush),
    ("NtFsControlFile", file::control),
    ("NtGetContextThread", thread::get_context),
    ("NtGetNextThread", information::next_thread),
    ("NtGetNlsSectionPtr", nls::section),
    ("NtInitializeNlsFiles", nls::initialize),
    ("NtMapViewOfSection", section::map_view),
    ("NtPowerInformation", system::power_information),
    ("NtProtectVirtualMemory", memory::protect),
    ("NtPulseEvent", sync::pulse_event),
    ("NtQueryAttributesFile", file::query_attributes),
    ("NtQueryDefaultUILanguage", nls::default_ui_language),
    ("NtQueryDirectoryObject", object::query_directory),
    ("NtQueryEvent", sync::query_event),
    ("NtQueryFullAttributesFile", file::query_full_attributes),
    ("NtQueryInstallUILanguage", nls::install_ui_language),
    ("NtQueryLicenseValue", registry::query_license),
    ("NtQueryMutant", sync::query_mutant),
    ("NtQuerySymbolicLinkObject", object::query_symlink),
    ("NtQuerySystemEnvironmentValue", system::query_environment),
    (
        "NtQuerySystemEnvironmentValueEx",
        system::query_environment_ex,
    ),
    ("NtQuerySystemInformationEx", system::query_ex),
    ("NtQueryTimerResolution", system::query_timer_resolution),
    ("NtQueueApcThread", sync::queue_apc),
    ("NtQueryDefaultLocale", nls::default_locale),
    ("NtQueryDirectoryFile", directory::query),
    ("NtQueryInformationAtom", atom::query),
    ("NtQueryInformationFile", file::query),
    ("NtQueryInformationProcess", information::query_process),
    ("NtQueryInformationThread", information::query_thread),
    ("NtQueryInformationToken", token::query),
    ("NtQueryKey", registry::query),
    ("NtQueryObject", object::query),
    ("NtQuerySection", section::query),
    ("NtQuerySemaphore", sync::query_semaphore),
    ("NtQuerySystemInformation", system::query),
    (
        "NtQueryPerformanceCounter",
        system::query_performance_counter,
    ),
    ("NtQuerySystemTime", system::query_time),
    ("NtQueryTimer", sync::query_timer),
    ("NtQueryValueKey", registry::query_value),
    ("NtQueryVirtualMemory", memory::query),
    ("NtQueueApcThreadEx2", sync::queue_apc_ex2),
    ("NtRaiseException", exception::raise),
    ("NtRaiseHardError", system::raise_hard_error),
    ("NtReadFile", file::read),
    ("NtReadRequestData", sync::port_stub),
    ("NtReadVirtualMemory", memory::read_virtual),
    ("NtReleaseKeyedEvent", sync::release_keyed_event),
    ("NtReleaseMutant", sync::release_mutant),
    ("NtReleaseSemaphore", sync::release_semaphore),
    ("NtRemoveIoCompletion", sync::remove_completion),
    ("NtRemoveIoCompletionEx", sync::remove_completion_ex),
    ("NtRemoveProcessDebug", debug::detach),
    ("NtReplyPort", sync::port_stub),
    ("NtReplyWaitReceivePort", sync::port_stub),
    ("NtReplyWaitReceivePortEx", sync::port_stub),
    ("NtRequestWaitReplyPort", sync::port_stub),
    ("NtResetEvent", sync::reset_event),
    ("NtResetWriteWatch", memory::reset_write_watch),
    ("NtResumeThread", thread::resume),
    ("NtRollbackTransaction", sync::rollback_transaction),
    ("NtSecureConnectPort", sync::port_stub),
    ("NtSetContextThread", thread::set_context),
    ("NtSetDebugFilterState", debug::set_filter),
    ("NtSetDefaultLocale", nls::set_default_locale),
    ("NtSetDefaultUILanguage", nls::set_ui_language),
    ("NtSetEvent", sync::set_event),
    ("NtSetEventBoostPriority", sync::boost_event),
    ("NtSetInformationDebugObject", sync::set_debug_object),
    ("NtSetInformationFile", file::set),
    ("NtSetInformationObject", object::set),
    ("NtSetInformationProcess", information::set_process),
    ("NtSetInformationThread", information::set_thread),
    ("NtSetInformationVirtualMemory", memory::set_information),
    ("NtSetIntervalProfile", sync::set_interval_profile),
    ("NtSetIoCompletion", sync::set_completion),
    ("NtSetLdtEntries", memory::set_ldt_entries),
    ("NtSetSystemInformation", system::set),
    ("NtSetSystemTime", system::set_time),
    ("NtSetThreadExecutionState", system::set_execution_state),
    ("NtSetTimer", sync::set_timer),
    ("NtSetTimerResolution", system::set_timer_resolution),
    ("NtSetValueKey", registry::set_value),
    ("NtShutdownSystem", system::shutdown),
    ("NtSignalAndWaitForSingleObject", sync::signal_and_wait),
    ("NtSystemDebugControl", system::debug_control),
    ("NtTerminateProcess", process::terminate),
    ("NtSuspendThread", thread::suspend),
    ("NtTerminateThread", thread::terminate),
    ("NtTestAlert", sync::test_alert),
    ("NtTraceControl", debug::trace_control),
    ("NtTraceEvent", debug::trace_event),
    ("NtUnloadDriver", system::unload_driver),
    ("NtUnlockVirtualMemory", memory::unlock_virtual),
    ("NtUnmapViewOfSection", section::unmap_view),
    ("NtUnmapViewOfSectionEx", section::unmap_view_ex),
    ("NtWaitForAlertByThreadId", sync::wait_alert),
    ("NtWaitForDebugEvent", sync::wait_debug_event),
    ("NtWaitForKeyedEvent", sync::wait_keyed_event),
    ("NtWaitForMultipleObjects", sync::wait_multiple),
    ("NtWaitForSingleObject", sync::wait_single),
    ("NtWorkerFactoryWorkerReady", sync::worker_ready),
    ("NtWow64AllocateVirtualMemory64", memory::allocate64),
    ("NtWow64GetNativeSystemInformation", system::query_native),
    (
        "NtWow64QueryInformationProcess64",
        information::query_process64,
    ),
    ("NtWow64ReadVirtualMemory64", memory::read_virtual64),
    ("NtWow64WriteVirtualMemory64", memory::write_virtual64),
    ("NtWriteFile", file::write),
    ("NtWriteRequestData", sync::port_stub),
    ("NtWriteVirtualMemory", memory::write_virtual),
    ("NtYieldExecution", sync::yield_execution),
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

/// The first number of the win32k table, which `win32u.so` registers.
const WIN32K_FIRST: usize = 0x1000;

/// No system call takes more arguments.
const MAX_ARGUMENTS: usize = 16;

/// The module the dispatchers are registered under; it stands in for
/// ntdll.so.
pub const MODULE: &str = "ntdll.so";
pub const SYSCALL: &str = "__wine_syscall_dispatcher";
pub const UNIX_CALL: &str = "__wine_unix_call_dispatcher";
/// ntdll.dll's own `__wine_unixlib_handle`, which serves its calls into
/// ntdll.so; no Unix library's table sits at this address.
pub const NTDLL_UNIXLIB: u32 = 1;

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
    if number >= WIN32K_FIRST {
        kuser::refresh(env);
        server::set_time(env);
        return win32k::call(env, number as u32);
    }
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
    let (handle, high, code, args) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    match (handle, high) {
        (NTDLL_UNIXLIB, 0) => unix::call(env, code, args),
        // Any other handle is a Unix library's table, which only i386
        // addresses fill.
        (funcs, 0) => unixlib::call(env, funcs, code, args),
        _ => Outcome::Return(crate::status::INVALID_HANDLE),
    }
}
