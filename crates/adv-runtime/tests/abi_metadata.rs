//! Register the production API fronts without guest images or host audio devices.
fn omission_reason(name: &str) -> &'static str {
    let (module, export) = name.split_once('!').expect("qualified export");
    if module == "msvcrt.dll" || module == "msvcr80.dll" || module == "msvcr90.dll" {
        "CRT outside Win32Metadata"
    } else if module == "vcruntime140.dll" && matches!(export, "memset" | "memcpy" | "memmove") {
        // These C runtime exports, like the legacy CRT exports above, are outside
        // Win32Metadata. Keep this classification exact; check its x86 ABI below.
        "CRT outside Win32Metadata"
    } else if module.contains("vorbis") {
        "codec ABI outside Windows SDK"
    } else if module == "vcomp140.dll" && vcomp_conv(export).is_some() {
        "OpenMP compiler runtime outside Win32Metadata"
    } else if export.starts_with('#') {
        "ordinal alias (WinMD is name-based)"
    } else if export.starts_with('<')
        || export.starts_with("__pal_")
        || module == "<win32>"
            && matches!(
                export,
                "startup_enter"
                    | "startup_leave"
                    | "thread_start"
                    | "thread_exception_handler"
                    | "dll_init"
            )
    {
        "personality-internal guest entry"
    } else if module == "kernelbase.dll"
        && adv_abi::signature::lookup("kernel32.dll", export).is_some()
    {
        "KernelBase export with kernel32 SDK signature"
    } else if module == "kernel32.dll" && export == "GetEnvironmentStringsA" {
        "SDK/export name alias"
    } else if module == "kernel32.dll"
        && [
            "InterlockedCompareExchange",
            "InterlockedDecrement",
            "InterlockedExchange",
            "InterlockedExchangeAdd",
            "InterlockedIncrement",
        ]
        .contains(&export)
    {
        "SDK intrinsic absent from PInvoke metadata"
    } else if module == "ntdll.dll" && export == "RtlFormatCurrentUserKeyPath" {
        // This one legacy export is absent from the pinned WinMD. Microsoft's
        // historical Ntrtl.h documentation specifies NTSTATUS WINAPI and one
        // CurrentUserKeyPath argument:
        // https://learn.microsoft.com/en-us/previous-versions/ff899322(v=vs.85)
        // SDK 10.0.26100.0 um/x86/ntdll.lib independently records
        // _RtlFormatCurrentUserKeyPath@4. Do not invent a full metadata signature.
        "documented legacy RTL export absent from pinned WinMD"
    } else if module == "ntdll.dll"
        && matches!(
            export,
            "LdrInitializeThunk" | "RtlUserThreadStart" | "KiUserExceptionDispatcher"
        )
        || module == "kernel32.dll" && export == "BaseThreadInitThunk"
    {
        "personality-owned x86 startup entry (initial register contract)"
    } else if module == "ntdll.dll" && native_argc(export).is_some() {
        "NT native API (WDK, outside the Win32 WinMD)"
    } else {
        "uncategorized metadata gap"
    }
}

// Wine's dlls/vcomp/vcomp.spec records these x86 contracts; the authored native
// tools/fixtures/vcomp-serial.c probe verifies the installed Microsoft runtime.
// Keep the scope exact, including the three fixed words before fork varargs.
fn vcomp_conv(export: &str) -> Option<adv_abi::Conv> {
    use adv_abi::Conv;
    match export {
        "_vcomp_fork" => Some(Conv::CdeclVariadic { fixed: 3 }),
        "_vcomp_for_static_simple_init" => Some(Conv::Cdecl { argc: 6 }),
        "_vcomp_for_static_end" => Some(Conv::Cdecl { argc: 0 }),
        _ => None,
    }
}

/// x86 native contracts verified against SDK 10.0.26100.0 decorated imports
/// and public System Informer phnt declarations for newer thread/APC entries.
/// Loader, RTL synchronization and named-object additions also checked against
/// https://github.com/winsiderss/phnt/blob/master/ntldr.h,
/// https://github.com/winsiderss/phnt/blob/master/ntrtl.h and
/// https://github.com/winsiderss/phnt/blob/master/ntexapi.h.
/// Zw aliases share the corresponding Nt contract.
fn native_argc(export: &str) -> Option<u8> {
    match export {
        "LdrUnloadDll" => return Some(1),
        "LdrGetDllHandle" => return Some(4),
        "LdrGetDllHandleEx" => return Some(5),
        "LdrAddRefDll" | "RtlPcToFileHeader" => return Some(2),
        "RtlGetVersion"
        | "RtlSetUnhandledExceptionFilter"
        | "RtlInitializeSRWLock"
        | "RtlAcquireSRWLockExclusive"
        | "RtlAcquireSRWLockShared"
        | "RtlReleaseSRWLockExclusive"
        | "RtlReleaseSRWLockShared"
        | "RtlTryAcquireSRWLockExclusive"
        | "RtlTryAcquireSRWLockShared"
        | "RtlInitializeConditionVariable"
        | "RtlWakeConditionVariable"
        | "RtlWakeAllConditionVariable" => return Some(1),
        "RtlCompareMemory" | "RtlSleepConditionVariableCS" => return Some(3),
        "RtlSleepConditionVariableSRW" => return Some(4),
        "RtlRaiseException" | "RtlRemoveVectoredExceptionHandler" => return Some(1),
        "RtlAddVectoredExceptionHandler" | "RtlInterlockedPushEntrySList" => return Some(2),
        "RtlEncodePointer"
        | "RtlDecodePointer"
        | "RtlInitializeSListHead"
        | "RtlInterlockedPopEntrySList"
        | "RtlInterlockedFlushSList"
        | "RtlQueryDepthSList" => return Some(1),
        "RtlDispatchAPC" => return Some(3),
        "RtlExitUserThread" => return Some(1),
        "RtlCreateUserThread" => return Some(10),
        "RtlReAllocateHeap" => return Some(4),
        "RtlSizeHeap" => return Some(3),
        "RtlInitializeCriticalSection"
        | "RtlDeleteCriticalSection"
        | "RtlEnterCriticalSection"
        | "RtlTryEnterCriticalSection"
        | "RtlLeaveCriticalSection" => return Some(1),
        _ => {}
    }
    let name = export
        .strip_prefix("Nt")
        .or_else(|| export.strip_prefix("Zw"))?;
    let bytes = match name {
        "Close" => 4,
        "CreateMutant" => 16,
        "OpenMutant" | "OpenSemaphore" | "ReleaseSemaphore" => 12,
        "ReleaseMutant" => 8,
        "CreateSemaphore" => 20,
        "Continue" | "FlushBuffersFile" => 8,
        "QueueApcThread" => 20,
        "TestAlert" => 0,
        "WaitForSingleObject" | "OpenEvent" | "RaiseException" => 12,
        "CreateEvent" | "WaitForMultipleObjects" => 20,
        "SetEvent" | "ResetEvent" => 8,
        "QueryVolumeInformationFile" => 20,
        "QueryDirectoryFile" => 44,
        "CreateThreadEx" => 44,
        "SuspendThread" | "TerminateThread" => 8,
        "CreateSection" => 28,
        "MapViewOfSection" => 40,
        "OpenFile" => 24,
        "ProtectVirtualMemory" => 20,
        "QueryInformationProcess" => 20,
        "QuerySystemInformation" => 16,
        "SetInformationThread" => 16,
        "UnmapViewOfSection" => 8,
        _ => return None,
    };
    Some(bytes / 4)
}

#[test]
// spec: PERS-ABI-1
fn all_fronts_signature_audit() {
    let mut report = vec!["# policy\tqualified export\tomission category".to_owned()];
    for policy in [
        adv_runtime::vorbis::Policy::Guest,
        adv_runtime::vorbis::Policy::Native,
    ] {
        let registry = adv_runtime::boot::api_registry(policy);
        let conflicts = registry.signature_conflicts();
        eprintln!(
            "{policy:?}: known runtime ABI conflicts: {}",
            conflicts.len()
        );
        for conflict in &conflicts {
            eprintln!(
                "{}!{}: {:?} vs {:?}",
                conflict.module, conflict.name, conflict.registered, conflict.metadata
            );
        }
        assert!(
            conflicts.is_empty(),
            "real x86 signature conflicts must be fixed, never suppressed: {conflicts:?}"
        );
        // The exact omission above still has an independently checked x86 ABI.
        assert_eq!(
            registry
                .get("ntdll.dll", "RtlFormatCurrentUserKeyPath")
                .unwrap()
                .conv,
            adv_abi::Conv::Stdcall { argc: 1 }
        );
        for name in ["memset", "memcpy", "memmove"] {
            assert_eq!(
                registry.get("vcruntime140.dll", name).unwrap().conv,
                adv_abi::Conv::Cdecl { argc: 3 },
                "vcruntime140.dll!{name}"
            );
        }
        let unknown = registry.missing_signatures();
        let mut categories = std::collections::BTreeMap::new();
        let mut uncategorized = Vec::new();
        for name in &unknown {
            let reason = omission_reason(name);
            if reason == "uncategorized metadata gap" {
                uncategorized.push(name);
            }
            if let Some(export) = name.strip_prefix("kernelbase.dll!") {
                if let Some(signature) = adv_abi::signature::lookup("kernel32.dll", export) {
                    assert_eq!(
                        Some(registry.get("kernelbase.dll", export).unwrap().conv),
                        signature.conv,
                        "{name}"
                    );
                }
            }
            if let Some(conv) = name.strip_prefix("vcomp140.dll!").and_then(vcomp_conv) {
                let export = name.split_once('!').unwrap().1;
                assert_eq!(
                    registry.get("vcomp140.dll", export).unwrap().conv,
                    conv,
                    "{name}"
                );
            }
            if let Some(argc) = native_argc(name.trim_start_matches("ntdll.dll!")) {
                assert_eq!(
                    registry
                        .get("ntdll.dll", &name["ntdll.dll!".len()..])
                        .unwrap()
                        .conv,
                    adv_abi::Conv::Stdcall { argc },
                    "{name}"
                );
            }
            *categories.entry(reason).or_insert(0usize) += 1;
            report.push(format!("{policy:?}\t{name}\t{reason}"));
        }
        assert!(
            uncategorized.is_empty(),
            "new omissions need investigation: {uncategorized:?}"
        );
        eprintln!(
            "{policy:?}: unknown signatures {}: {categories:?}",
            unknown.len()
        );
        assert_eq!(
            registry.get("user32.dll", "wsprintfA").unwrap().conv,
            adv_abi::Conv::CdeclVariadic { fixed: 2 }
        );
        assert_eq!(
            registry
                .get("dsound.dll", "IDirectSoundBuffer8::Initialize")
                .unwrap()
                .conv,
            adv_abi::Conv::Stdcall { argc: 3 }
        );
    }
    if let Some(path) = std::env::var_os("ADV_ABI_AUDIT_PATH") {
        std::fs::write(path, report.join("\n") + "\n").unwrap();
    }
}

#[test]
fn diagnostic_override_has_the_same_variadic_contract() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "all_fronts_signature_audit", "--nocapture"])
        .env("ADV_PRINTF_DIAG_PATH", "abi-audit-never-executed.log")
        .env_remove("ADV_ABI_AUDIT_PATH")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
#[test]
fn gdi_object_extent_and_text_output_bindings_are_generated_without_dispatch_data() {
    let registry = adv_runtime::boot::api_registry(adv_runtime::vorbis::Policy::Guest);
    for name in [
        "GetStockObject",
        "CreateFontIndirectA",
        "CreateFontA",
        "CreateSolidBrush",
        "CreatePen",
        "GetObjectA",
        "CreateBitmap",
        "CreateCompatibleBitmap",
        "GetDeviceCaps",
        "SetTextColor",
        "SetBkColor",
        "SetBkMode",
        "GetTextExtentPoint32A",
        "GetTextExtentPoint32W",
    ] {
        assert!(
            registry.level("gdi32.dll", name).is_some(),
            "{name} must be a generated descriptor"
        );
        assert!(registry.is_implemented("gdi32.dll", name));
        assert_eq!(registry.get("gdi32.dll", name).unwrap().data, 0);
    }
    for name in [
        "TextOutA",
        "TextOutW",
        "ExtTextOutA",
        "ExtTextOutW",
        "SetTextAlign",
        "GetTextAlign",
    ] {
        assert!(registry.is_implemented("gdi32.dll", name));
        assert_eq!(registry.get("gdi32.dll", name).unwrap().data, 0);
    }
    assert_eq!(
        registry.get("gdi32.dll", "TextOutA").unwrap().conv,
        adv_abi::Conv::Stdcall { argc: 5 }
    );
}

#[test]
fn shader_and_effect_fronts_match_the_complete_sdk_contract() {
    let registry = adv_runtime::boot::api_registry(adv_runtime::vorbis::Policy::Guest);
    let buffers: Vec<_> = adv_abi::signature::com_signatures()
        .iter()
        .filter(|s| s.module == "ID3DXBuffer")
        .collect();
    assert_eq!(buffers.len(), 5);
    for (slot, (signature, name)) in buffers
        .iter()
        .zip([
            "QueryInterface",
            "AddRef",
            "Release",
            "GetBufferPointer",
            "GetBufferSize",
        ])
        .enumerate()
    {
        assert_eq!(signature.com_slot, Some(slot as u16));
        assert_eq!(signature.name, name);
        assert_eq!(
            signature.conv,
            Some(adv_abi::Conv::Stdcall {
                argc: if slot == 0 { 3 } else { 1 },
            })
        );
        let host = registry
            .get("d3dx9_43.dll", &format!("ID3DXBuffer::{name}"))
            .unwrap();
        assert_eq!(Some(host.conv), signature.conv);
    }
    assert_eq!(buffers[3].return_kind, adv_abi::AbiKind::Pointer);
    assert_eq!(buffers[4].return_kind, adv_abi::AbiKind::U32);
    let effects: Vec<_> = adv_abi::signature::com_signatures()
        .iter()
        .filter(|s| s.module == "ID3DXEffect")
        .collect();
    assert_eq!(effects.len(), 79);
    for (slot, signature) in effects.iter().enumerate() {
        assert_eq!(signature.com_slot, Some(slot as u16));
        let name = format!("ID3DXEffect::{}", signature.name);
        let host = registry
            .get("d3dx9_43.dll", &name)
            .expect("effect slot registered");
        assert_eq!(Some(host.conv), signature.conv, "{name}");
        assert!(registry.level("d3dx9_43.dll", &name).is_some());
    }
    for name in [
        "D3DXCreateEffect",
        "D3DXGetShaderOutputSemantics",
        "D3DXSaveSurfaceToFileInMemory",
        "D3DXSaveSurfaceToFileW",
    ] {
        let signature = adv_abi::signature::lookup("d3dx9_43.dll", name).unwrap();
        let host = registry.get("d3dx9_43.dll", name).unwrap();
        assert_eq!(Some(host.conv), signature.conv, "{name}");
        assert!(registry.is_implemented("d3dx9_43.dll", name));
        if name.starts_with("D3DXSaveSurface") {
            assert_eq!(signature.conv, Some(adv_abi::Conv::Stdcall { argc: 5 }));
            assert_eq!(signature.return_kind, adv_abi::AbiKind::I32);
            assert_eq!(signature.parameters.len(), 5);
        }
    }
    #[cfg(feature = "gpu")]
    {
        let shaders: Vec<_> = adv_abi::signature::com_signatures()
            .iter()
            .filter(|s| {
                (s.module == "IDirect3DDevice9" && s.name.contains("Shader"))
                    || matches!(s.module, "IDirect3DVertexShader9" | "IDirect3DPixelShader9")
            })
            .collect();
        assert_eq!(shaders.len(), 28);
        for signature in shaders {
            let name = format!("{}::{}", signature.module, signature.name);
            let host = registry
                .get("d3d9.dll", &name)
                .expect("shader entry registered");
            assert_eq!(Some(host.conv), signature.conv, "{name}");
            assert!(registry.is_implemented("d3d9.dll", &name));
        }
    }
}
