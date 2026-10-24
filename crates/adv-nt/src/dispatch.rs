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
    ("NtAccessCheck", object::access_check),
    ("NtAccessCheckAndAuditAlarm", object::access_check_audit),
    (
        "NtAccessCheckByTypeAndAuditAlarm",
        object::access_check_by_type_audit,
    ),
    ("NtAddAtom", atom::add),
    ("NtAdjustGroupsToken", token::adjust_groups),
    ("NtAdjustPrivilegesToken", token::adjust_privileges),
    ("NtAlertMultipleThreadByThreadId", sync::alert_multiple),
    ("NtAlertResumeThread", thread::resume),
    ("NtAlertThread", thread::not_implemented),
    ("NtAlertThreadByThreadId", sync::alert_thread),
    (
        "NtAllocateLocallyUniqueId",
        token::allocate_locally_unique_id,
    ),
    ("NtAllocateReserveObject", object::allocate_reserve),
    ("NtAllocateUuids", token::allocate_uuids),
    ("NtAllocateVirtualMemory", memory::allocate),
    ("NtAllocateVirtualMemoryEx", memory::allocate_ex),
    ("NtApphelpCacheControl", exception::syscall_stub),
    ("NtAreMappedFilesTheSame", section::are_same),
    ("NtAssignProcessToJobObject", sync::assign_job),
    ("NtCallbackReturn", win32k::callback_return),
    ("NtCancelIoFile", file::cancel),
    ("NtCancelIoFileEx", file::cancel_ex),
    ("NtCancelSynchronousIoFile", file::cancel_sync),
    ("NtCancelTimer", sync::cancel_timer),
    ("NtClearEvent", sync::clear_event),
    ("NtClose", object::close),
    ("NtCloseObjectAuditAlarm", object::close_audit),
    ("NtCommitTransaction", sync::commit_transaction),
    ("NtCompareObjects", object::compare),
    ("NtCompareTokens", token::compare),
    ("NtCompleteConnectPort", sync::port_stub),
    ("NtConnectPort", sync::port_stub),
    ("NtContinue", exception::resume),
    ("NtContinueEx", exception::resume_ex),
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
    ("NtCreateKeyTransacted", registry::create_transacted),
    ("NtCreateKeyedEvent", sync::create_keyed_event),
    ("NtCreateLowBoxToken", token::create_low_box),
    ("NtCreateMailslotFile", file::create_mailslot),
    ("NtCreateMutant", sync::create_mutant),
    ("NtCreateNamedPipeFile", file::create_named_pipe),
    ("NtCreatePagingFile", memory::create_paging_file),
    ("NtCreatePort", sync::port_stub),
    ("NtCreateProcessEx", exception::syscall_stub),
    ("NtCreateSection", section::create),
    ("NtCreateSectionEx", section::create_ex),
    ("NtCreateSemaphore", sync::create_semaphore),
    ("NtCreateSymbolicLinkObject", object::create_symlink),
    ("NtCreateThread", thread::not_implemented),
    ("NtCreateThreadEx", thread::create),
    ("NtCreateTimer", sync::create_timer),
    ("NtCreateToken", token::create),
    ("NtCreateTransaction", sync::create_transaction),
    ("NtCreateUserProcess", process::create_user_process),
    ("NtDebugActiveProcess", debug::attach),
    ("NtDebugContinue", debug::continue_event),
    ("NtDelayExecution", sync::delay),
    ("NtDeleteAtom", atom::delete),
    ("NtDeleteFile", file::delete),
    ("NtDeleteKey", registry::delete),
    ("NtDeleteValueKey", registry::delete_value),
    ("NtDeviceIoControlFile", file::device_control),
    ("NtDisplayString", system::display_string),
    ("NtDuplicateObject", object::duplicate),
    ("NtDuplicateToken", token::duplicate),
    ("NtEnumerateKey", registry::enumerate),
    ("NtEnumerateValueKey", registry::enumerate_value),
    ("NtFilterToken", token::filter),
    ("NtFindAtom", atom::find),
    ("NtFlushBuffersFile", file::flush),
    ("NtFlushBuffersFileEx", file::flush_ex),
    ("NtFlushInstructionCache", memory::flush_instruction_cache),
    ("NtFlushKey", registry::flush),
    ("NtFlushProcessWriteBuffers", memory::flush_write_buffers),
    ("NtFlushVirtualMemory", memory::flush_virtual),
    ("NtFreeVirtualMemory", memory::free),
    ("NtFsControlFile", file::control),
    ("NtGetContextThread", thread::get_context),
    ("NtGetCurrentProcessorNumber", thread::processor_number),
    ("NtGetNextProcess", information::next_process),
    ("NtGetNextThread", information::next_thread),
    ("NtGetNlsSectionPtr", nls::section),
    ("NtGetWriteWatch", memory::get_write_watch),
    ("NtImpersonateAnonymousToken", token::impersonate_anonymous),
    ("NtImpersonateClientOfPort", sync::port_stub),
    ("NtInitializeNlsFiles", nls::initialize),
    ("NtInitiatePowerAction", system::initiate_power_action),
    ("NtIsProcessInJob", sync::process_in_job),
    ("NtListenPort", sync::port_stub),
    ("NtLoadDriver", system::load_driver),
    ("NtLoadKey", registry::load),
    ("NtLoadKey2", registry::load),
    ("NtLoadKeyEx", registry::load),
    ("NtLockFile", file::lock),
    ("NtLockVirtualMemory", memory::lock_virtual),
    ("NtMakePermanentObject", object::make_permanent),
    ("NtMakeTemporaryObject", object::make_temporary),
    ("NtMapUserPhysicalPagesScatter", exception::syscall_stub),
    ("NtMapViewOfSection", section::map_view),
    ("NtMapViewOfSectionEx", section::map_view_ex),
    ("NtNotifyChangeDirectoryFile", file::notify_change),
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
    ("NtOpenKeyTransacted", registry::open),
    ("NtOpenKeyTransactedEx", registry::open_ex),
    ("NtOpenKeyedEvent", sync::open_keyed_event),
    ("NtOpenMutant", sync::open_mutant),
    ("NtOpenProcess", information::open_process),
    ("NtOpenProcessToken", token::open_process),
    ("NtOpenProcessTokenEx", token::open_process_ex),
    ("NtOpenSection", section::open),
    ("NtOpenSemaphore", sync::open_semaphore),
    ("NtOpenSymbolicLinkObject", object::open_symlink),
    ("NtOpenThread", information::open_thread),
    ("NtOpenThreadToken", token::open_thread),
    ("NtOpenThreadTokenEx", token::open_thread_ex),
    ("NtOpenTimer", sync::open_timer),
    ("NtPowerInformation", system::power_information),
    ("NtPrivilegeCheck", token::privilege_check),
    ("NtProtectVirtualMemory", memory::protect),
    ("NtPulseEvent", sync::pulse_event),
    ("NtQueryAttributesFile", file::query_attributes),
    ("NtQueryDefaultLocale", nls::default_locale),
    ("NtQueryDefaultUILanguage", nls::default_ui_language),
    ("NtQueryDirectoryFile", directory::query),
    ("NtQueryDirectoryObject", object::query_directory),
    ("NtQueryEaFile", file::query_ea),
    ("NtQueryEvent", sync::query_event),
    ("NtQueryFullAttributesFile", file::query_full_attributes),
    ("NtQueryInformationAtom", atom::query),
    ("NtQueryInformationFile", file::query),
    ("NtQueryInformationJobObject", sync::query_job),
    ("NtQueryInformationProcess", information::query_process),
    ("NtQueryInformationThread", information::query_thread),
    ("NtQueryInformationToken", token::query),
    ("NtQueryInstallUILanguage", nls::install_ui_language),
    ("NtQueryIoCompletion", sync::query_completion),
    ("NtQueryKey", registry::query),
    ("NtQueryLicenseValue", registry::query_license),
    ("NtQueryMultipleValueKey", registry::stub_success),
    ("NtQueryMutant", sync::query_mutant),
    ("NtQueryObject", object::query),
    (
        "NtQueryPerformanceCounter",
        system::query_performance_counter,
    ),
    ("NtQuerySection", section::query),
    ("NtQuerySecurityObject", object::query_security),
    ("NtQuerySemaphore", sync::query_semaphore),
    ("NtQuerySymbolicLinkObject", object::query_symlink),
    ("NtQuerySystemEnvironmentValue", system::query_environment),
    (
        "NtQuerySystemEnvironmentValueEx",
        system::query_environment_ex,
    ),
    ("NtQuerySystemInformation", system::query),
    ("NtQuerySystemInformationEx", system::query_ex),
    ("NtQuerySystemTime", system::query_time),
    ("NtQueryTimer", sync::query_timer),
    ("NtQueryTimerResolution", system::query_timer_resolution),
    ("NtQueryValueKey", registry::query_value),
    ("NtQueryVirtualMemory", memory::query),
    ("NtQueryVolumeInformationFile", file::query_volume),
    ("NtQueueApcThread", sync::queue_apc),
    ("NtQueueApcThreadEx", sync::queue_apc_ex),
    ("NtQueueApcThreadEx2", sync::queue_apc_ex2),
    ("NtRaiseException", exception::raise),
    ("NtRaiseHardError", system::raise_hard_error),
    ("NtReadFile", file::read),
    ("NtReadFileScatter", file::read_scatter),
    ("NtReadRequestData", sync::port_stub),
    ("NtReadVirtualMemory", memory::read_virtual),
    ("NtRegisterThreadTerminatePort", thread::not_implemented),
    ("NtReleaseKeyedEvent", sync::release_keyed_event),
    ("NtReleaseMutant", sync::release_mutant),
    ("NtReleaseSemaphore", sync::release_semaphore),
    ("NtRemoveIoCompletion", sync::remove_completion),
    ("NtRemoveIoCompletionEx", sync::remove_completion_ex),
    ("NtRemoveProcessDebug", debug::detach),
    ("NtRenameKey", registry::rename),
    ("NtReplaceKey", registry::stub_success),
    ("NtReplyPort", sync::port_stub),
    ("NtReplyWaitReceivePort", sync::port_stub),
    ("NtReplyWaitReceivePortEx", sync::port_stub),
    ("NtRequestWaitReplyPort", sync::port_stub),
    ("NtResetEvent", sync::reset_event),
    ("NtResetWriteWatch", memory::reset_write_watch),
    ("NtRestoreKey", registry::stub_success),
    ("NtResumeProcess", process::resume),
    ("NtResumeThread", thread::resume),
    ("NtRollbackTransaction", sync::rollback_transaction),
    ("NtSaveKey", registry::save),
    ("NtSecureConnectPort", sync::port_stub),
    ("NtSetContextThread", thread::set_context),
    ("NtSetDebugFilterState", debug::set_filter),
    ("NtSetDefaultLocale", nls::set_default_locale),
    ("NtSetDefaultUILanguage", nls::set_ui_language),
    ("NtSetEaFile", file::set_ea),
    ("NtSetEvent", sync::set_event),
    ("NtSetEventBoostPriority", sync::boost_event),
    ("NtSetInformationDebugObject", sync::set_debug_object),
    ("NtSetInformationFile", file::set),
    ("NtSetInformationJobObject", sync::set_job),
    ("NtSetInformationKey", registry::stub_success),
    ("NtSetInformationObject", object::set),
    ("NtSetInformationProcess", information::set_process),
    ("NtSetInformationThread", information::set_thread),
    ("NtSetInformationToken", token::set),
    ("NtSetInformationVirtualMemory", memory::set_information),
    ("NtSetIntervalProfile", sync::set_interval_profile),
    ("NtSetIoCompletion", sync::set_completion),
    ("NtSetIoCompletionEx", sync::set_completion_ex),
    ("NtSetLdtEntries", memory::set_ldt_entries),
    ("NtSetSecurityObject", object::set_security),
    ("NtSetSystemInformation", system::set),
    ("NtSetSystemTime", system::set_time),
    ("NtSetThreadExecutionState", system::set_execution_state),
    ("NtSetTimer", sync::set_timer),
    ("NtSetTimerResolution", system::set_timer_resolution),
    ("NtSetValueKey", registry::set_value),
    ("NtSetVolumeInformationFile", file::set_volume),
    ("NtShutdownSystem", system::shutdown),
    ("NtSignalAndWaitForSingleObject", sync::signal_and_wait),
    ("NtSuspendProcess", process::suspend),
    ("NtSuspendThread", thread::suspend),
    ("NtSystemDebugControl", system::debug_control),
    ("NtTerminateJobObject", sync::terminate_job),
    ("NtTerminateProcess", process::terminate),
    ("NtTerminateThread", thread::terminate),
    ("NtTestAlert", sync::test_alert),
    ("NtTraceControl", debug::trace_control),
    ("NtTraceEvent", exception::syscall_stub),
    ("NtUnloadDriver", system::unload_driver),
    ("NtUnloadKey", registry::unload),
    ("NtUnlockFile", file::unlock),
    ("NtUnlockVirtualMemory", memory::unlock_virtual),
    ("NtUnmapViewOfSection", section::unmap_view),
    ("NtUnmapViewOfSectionEx", section::unmap_view_ex),
    ("NtWaitForAlertByThreadId", sync::wait_alert),
    ("NtWaitForDebugEvent", sync::wait_debug_event),
    ("NtWaitForKeyedEvent", sync::wait_keyed_event),
    ("NtWaitForMultipleObjects", sync::wait_multiple),
    ("NtWaitForMultipleObjects32", exception::syscall_stub),
    ("NtWaitForSingleObject", sync::wait_single),
    ("NtWorkerFactoryWorkerReady", exception::syscall_stub),
    ("NtWow64AllocateVirtualMemory64", memory::allocate64),
    ("NtWow64GetNativeSystemInformation", system::query_native),
    (
        "NtWow64IsProcessorFeaturePresent",
        kuser::is_processor_feature_present,
    ),
    (
        "NtWow64QueryInformationProcess64",
        information::query_process64,
    ),
    ("NtWow64ReadVirtualMemory64", memory::read_virtual64),
    ("NtWow64WriteVirtualMemory64", memory::write_virtual64),
    ("NtWriteFile", file::write),
    ("NtWriteFileGather", file::write_gather),
    ("NtWriteRequestData", sync::port_stub),
    ("NtWriteVirtualMemory", memory::write_virtual),
    ("NtYieldExecution", sync::yield_execution),
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
    let outcome = call(env, &args[..count]);
    server::boundary(env, outcome)
}

fn unix_call(env: &mut Env<'_>) -> Outcome {
    let (handle, high, code, args) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let outcome = match (handle, high) {
        (NTDLL_UNIXLIB, 0) => unix::call(env, code, args),
        // Any other handle is a Unix library's table, which only i386
        // addresses fill.
        (funcs, 0) => unixlib::call(env, funcs, code, args),
        _ => Outcome::Return(crate::status::INVALID_HANDLE),
    };
    server::boundary(env, outcome)
}
