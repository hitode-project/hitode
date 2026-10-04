//! Register the production API fronts without guest images or host audio devices.
fn omission_reason(name: &str) -> &'static str {
    let (module, export) = name.split_once('!').expect("qualified export");
    if module == "msvcrt.dll" || module == "msvcr80.dll" || module == "msvcr90.dll" {
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
    } else {
        "uncategorized metadata gap"
    }
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
        let unknown = registry.missing_signatures();
        let mut categories = std::collections::BTreeMap::new();
        for name in &unknown {
            let reason = omission_reason(name);
            assert_ne!(
                reason, "uncategorized metadata gap",
                "new omission needs investigation: {name}"
            );
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
fn gdi_object_and_extent_bindings_are_generated_but_missing_textout_remains_a_gap() {
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
    assert!(!registry.is_implemented("gdi32.dll", "TextOutA"));
    assert_eq!(
        registry.get("gdi32.dll", "TextOutA").unwrap().conv,
        adv_abi::Conv::Stdcall { argc: 5 }
    );
}
