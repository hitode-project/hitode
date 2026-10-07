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
    } else if export.starts_with('#') {
        "ordinal alias (WinMD is name-based)"
    } else if export.starts_with('<')
        || export.starts_with("__pal_")
        || module == "<win32>"
            && matches!(export, "startup_enter" | "startup_leave" | "thread_start")
    {
        "personality-internal guest entry"
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
    } else if module == "ntdll.dll" && native_argc(export).is_some() {
        "NT native API (WDK, outside the Win32 WinMD)"
    } else {
        "uncategorized metadata gap"
    }
}

/// x86 stdcall argument counts of the NT native APIs, from the decorated
/// `_Name@bytes` imports in SDK 10.0.26100.0 um/x86/ntdll.lib. Zw aliases share them.
fn native_argc(export: &str) -> Option<u8> {
    let name = export
        .strip_prefix("Nt")
        .or_else(|| export.strip_prefix("Zw"))?;
    let bytes = match name {
        "Close" => 4,
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
        for name in &unknown {
            let reason = omission_reason(name);
            assert_ne!(
                reason, "uncategorized metadata gap",
                "new omission needs investigation: {name}"
            );
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
    for name in ["D3DXCreateEffect", "D3DXGetShaderOutputSemantics"] {
        let signature = adv_abi::signature::lookup("d3dx9_43.dll", name).unwrap();
        let host = registry.get("d3dx9_43.dll", name).unwrap();
        assert_eq!(Some(host.conv), signature.conv, "{name}");
        assert!(registry.is_implemented("d3dx9_43.dll", name));
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
