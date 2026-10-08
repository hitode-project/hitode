//! Authored structure checks for the fixed x86 system personality catalog.
use adv_abi::{ApiDiagnostics, Conv, Registry, system_exports};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot},
};
use adv_machine::{Clock, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_os::{FileSource, Loader, Modules, Process};
use adv_pe::{ExportTarget, ImportName};

struct Empty;
impl FileSource for Empty {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}

fn machine(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    registry.add(
        "ntdll.dll",
        "RtlAllocateHeap",
        Conv::Stdcall { argc: 3 },
        |_| Outcome::Return(17),
    );
    registry.add(
        "ntdll.dll",
        "RtlEnterCriticalSection",
        Conv::Stdcall { argc: 1 },
        |_| Outcome::Return(0),
    );
    registry.add(
        "kernel32.dll",
        "GetCurrentProcess",
        Conv::Stdcall { argc: 0 },
        |_| Outcome::Return(u32::MAX),
    );
    registry.add(
        "kernel32.dll",
        "InitOnceBeginInitialize",
        Conv::Stdcall { argc: 4 },
        |_| Outcome::Return(1),
    );
    registry.add(
        "<win32>",
        "thread_exception_handler",
        Conv::Stdcall { argc: 4 },
        |_| Outcome::Return(1),
    );
    Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap()
}

fn resolve(
    machine: &mut Machine<JitCpu>,
    module: usize,
    name: &str,
) -> Result<u32, adv_os::LoadError> {
    Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &Empty,
    )
    .resolve_dynamic(
        module,
        &ImportName::Name {
            hint: 0,
            name: name.into(),
        },
    )
    .map(|(address, _)| address)
}

const NT_NAMES: [&str; 56] = [
    "NtQuerySystemInformation",
    "NtQueryInformationProcess",
    "NtQueryInformationThread",
    "NtSetInformationThread",
    "NtCreateSection",
    "NtMapViewOfSection",
    "NtUnmapViewOfSection",
    "NtProtectVirtualMemory",
    "NtQueryVirtualMemory",
    "NtCreateEvent",
    "NtSetEvent",
    "NtWaitForSingleObject",
    "NtClose",
    "NtCreateFile",
    "NtOpenFile",
    "NtQueryDirectoryFile",
    "NtDeviceIoControlFile",
    "LdrGetDllHandle",
    "LdrLoadDll",
    "LdrGetProcedureAddress",
    "LdrFindEntryForAddress",
    "RtlPcToFileHeader",
    "RtlImageNtHeader",
    "RtlGetVersion",
    "RtlAddVectoredExceptionHandler",
    "RtlInitializeCriticalSection",
    "RtlEnterCriticalSection",
    "RtlLeaveCriticalSection",
    "RtlUserThreadStart",
    "KiUserExceptionDispatcher",
    "RtlCaptureContext",
    "RtlUnwind",
    "NtTestAlert",
    "NtQueueApcThread",
    "NtCreateThreadEx",
    "RtlCreateUserThread",
    "NtSuspendThread",
    "NtResumeThread",
    "NtGetContextThread",
    "RtlNtStatusToDosError",
    "RtlMultiByteToUnicodeN",
    "RtlUnicodeToMultiByteN",
    "LdrRegisterDllNotification",
    "NtYieldExecution",
    "NtOpenProcess",
    "RtlAllocateHeap",
    "RtlFreeHeap",
    "RtlGetLastWin32Error",
    "NtQueryPerformanceCounter",
    "RtlCompareMemory",
    "LdrInitializeThunk",
    "KiUserApcDispatcher",
    "NtContinue",
    "RtlRaiseException",
    "NtAllocateVirtualMemory",
    "RtlExitUserThread",
];

#[test]
fn fixed_names_are_image_code_and_unknown_names_remain_absent() {
    for jit in [false, true] {
        let mut machine = machine(jit);
        let ntdll = machine.load(&Empty, "ntdll.dll").unwrap();
        let image = adv_os::modules(&machine.ext).get(ntdll);
        let directory = image.base + machine.cpu.read_u32(image.base + 0x80 + 120).unwrap();
        assert_eq!(machine.cpu.read_u32(directory + 24).unwrap(), 2523);
        let mut count = 0;
        for name in NT_NAMES {
            let address = resolve(&mut machine, ntdll, name).unwrap();
            assert_eq!(
                adv_os::modules(&machine.ext).containing(address),
                Some(ntdll)
            );
            assert!(machine.cpu.accessible(address, 1, Prot::EXEC));
            count += 1;
        }
        assert_eq!(count, 56);
        assert!(resolve(&mut machine, ntdll, "UnknownOptionalExport").is_err());
        assert!(resolve(&mut machine, ntdll, "NlsAnsiCodePage").is_err());
        assert_eq!(
            system_exports::lookup("ntdll.dll", "NlsAnsiCodePage")
                .unwrap()
                .kind,
            system_exports::Kind::DataUnsupported
        );
        assert_eq!(machine.ext.get_or_default::<ApiDiagnostics>().stub_calls, 0);
        assert_eq!(
            adv_os::modules(&machine.ext).containing(machine.sys.guest_return_entry()),
            Some(ntdll)
        );
        assert_eq!(
            adv_os::modules(&machine.ext).containing(machine.sys.thread_exception_entry().unwrap()),
            Some(ntdll)
        );
    }
}

#[test]
fn native_forwarders_share_entries_and_api_sets_reach_kernelbase() {
    let mut machine = machine(false);
    let kernel = machine.load(&Empty, "kernel32.dll").unwrap();
    let native = machine.load(&Empty, "ntdll.dll").unwrap();
    for (source, target) in [
        ("HeapAlloc", "RtlAllocateHeap"),
        ("EnterCriticalSection", "RtlEnterCriticalSection"),
    ] {
        let address = resolve(&mut machine, kernel, source).unwrap();
        assert_eq!(address, resolve(&mut machine, native, target).unwrap());
        assert_eq!(
            adv_os::modules(&machine.ext).containing(address),
            Some(native)
        );
        assert!(matches!(
            adv_os::modules(&machine.ext).get(kernel).exports_by_name[source],
            ExportTarget::Forwarder(_)
        ));
    }
    let address = resolve(&mut machine, kernel, "InitOnceBeginInitialize").unwrap();
    let base = adv_os::modules(&machine.ext)
        .by_name("kernelbase.dll")
        .unwrap();
    assert_eq!(
        adv_os::modules(&machine.ext).containing(address),
        Some(base)
    );
    assert!(
        machine
            .sys
            .thunks
            .resolver()
            .implemented("kernelbase.dll", "InitOnceBeginInitialize")
    );
    let address = resolve(&mut machine, base, "GetCurrentProcess").unwrap();
    machine
        .start_main(0x400000, address, 65536, "authored")
        .unwrap();
    assert!(matches!(
        machine.run_virtual(100000),
        RunResult::Exited(u32::MAX)
    ));
}

#[test]
fn unknown_abi_fixed_exports_fail_by_name_without_returning() {
    for jit in [false, true] {
        let mut machine = machine(jit);
        let native = machine.load(&Empty, "ntdll.dll").unwrap();
        let name = "NtQueueApcThread";
        assert!(adv_abi::signature::lookup("ntdll.dll", name).is_none());
        let entry = resolve(&mut machine, native, name).unwrap();
        machine
            .start_main(0x400000, entry, 65536, "authored")
            .unwrap();
        let RunResult::Failed(failure) = machine.run_virtual(100000) else {
            panic!("fixed stub returned")
        };
        assert!(failure.message.contains("ntdll.dll!NtQueueApcThread"));
        let diagnostics = machine.ext.get::<ApiDiagnostics>();
        assert_eq!(diagnostics.stub_calls, 1);
        assert_eq!(diagnostics.recent_stubs[0].name, name);
        assert_eq!(diagnostics.recent_stubs[0].failure_value, None);
        assert!(!diagnostics.warnings.is_empty());
    }
}
