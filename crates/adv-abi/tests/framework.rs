use adv_abi::{
    ApiDiagnostics, Conv, Env, GuestPtr, ImplementationLevel, Outcome, Registry, RunMode,
    signature, win32,
};
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PAGE, PARK_ADDR, teb},
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;

#[test]
// spec: PERS-EXPORT-6
fn data_declarations_have_width_and_cannot_be_function_bindings() {
    use adv_machine::Resolver;
    let mut registry = Registry::default();
    let signature = signature::lookup("msvcr90.dll", "_acmdln").unwrap();
    assert_eq!(signature.data_size, Some(4));
    assert!(!signature.callable());
    assert!(signature.conv.is_none());
    assert!(signature.parameters.is_empty());
    assert_eq!(registry.data_size("msvcr90.dll", "_acmdln"), Some(4));
    assert!(registry.get("msvcr90.dll", "_acmdln").is_none());
    assert!(!registry.dynamic_available("msvcr90.dll", "_acmdln"));
    assert!(
        registry
            .register_data("msvcr90.dll", "_acmdln", &[0; 8])
            .is_err()
    );
    assert!(
        registry
            .register_data("kernel32.dll", "GetTickCount", &[0; 4])
            .is_err()
    );
    registry
        .register_data("MSVCR90.DLL", "_acmdln", &[0; 4])
        .unwrap();
    assert!(registry.dynamic_available("msvcr90.dll", "_acmdln"));
    assert_eq!(
        registry.level("msvcr90.dll", "_acmdln"),
        Some(ImplementationLevel::State)
    );
    assert_eq!(registry.coverage().state, 1);
    assert!(
        registry
            .set_data("msvcr90.dll", "_acmdln", &[0; 8])
            .is_err()
    );
    registry
        .set_data("msvcr90.dll", "_acmdln", &17u32.to_le_bytes())
        .unwrap();
    assert_eq!(
        registry.host_data("msvcr90.dll", "_acmdln"),
        Some(17u32.to_le_bytes().as_slice())
    );
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            registry.add("msvcr90.dll", "_acmdln", Conv::Cdecl { argc: 0 }, |_| {
                Outcome::Return(0)
            });
        }))
        .is_err()
    );
    assert!(registry.signature_conflicts().is_empty());
}

#[test]
// spec: PERS-EXPORT-2
fn dynamic_availability_exposes_fixed_names_only_with_capability() {
    let mut registry = Registry::default();
    registry.set_expose_fixed_exports(false);
    for mode in [RunMode::Strict, RunMode::Discovery] {
        registry.set_mode(mode);
        assert!(
            registry.get("kernel32.dll", "FlsAlloc").is_some(),
            "metadata retains a static diagnostic handler"
        );
        // Structure keeps the name; discovery hides it until it is callable.
        assert!(adv_abi::system_exports::is_core_export(
            "kernel32.dll",
            "FlsAlloc"
        ));
        assert!(!registry.dynamic_available("kernel32.dll", "FlsAlloc"));
        assert!(!registry.is_implemented("kernel32.dll", "FlsAlloc"));
        assert!(!registry.dynamic_available("kernel32.dll", "UnknownOptionalExport"));
        assert!(registry.dynamic_available("DWRITE.DLL", "DWriteCreateFactory"));
        assert!(registry.dynamic_available("d2d1.dll", "#1"));
        assert!(!registry.dynamic_available("kernel32.dll", "CreateFileW"));
        assert!(!registry.dynamic_available("d3d11.dll", "D3D11CreateDeviceAndSwapChain"));
    }
    // ADV_PERSONALITY_STRICT restores whole-catalog discovery, never data stubs.
    registry.set_expose_fixed_exports(true);
    assert!(registry.dynamic_available("kernel32.dll", "FlsAlloc"));
    assert!(!registry.dynamic_available("kernel32.dll", "UnknownOptionalExport"));
    registry.set_expose_fixed_exports(false);
    registry.register(adv_abi::Api {
        module: "kernel32.dll",
        name: "FlsAlloc",
        conv: Conv::Stdcall { argc: 1 },
        env_leaf: None,
        handler: |_| Outcome::Return(u32::MAX),
        level: ImplementationLevel::SafeFail,
    });
    assert!(registry.dynamic_available("kernel32.dll", "FlsAlloc"));
    assert_eq!(
        registry.level("kernel32.dll", "FlsAlloc"),
        Some(ImplementationLevel::SafeFail)
    );
}

#[test]
// spec: PERS-EXPORT-2
fn a_frontend_under_a_forwarding_name_implements_its_fixed_target() {
    let mut registry = Registry::default();
    registry.set_expose_fixed_exports(false);
    assert_eq!(
        adv_abi::system_exports::final_target("kernel32.dll", "SetThreadpoolTimer"),
        Some(("ntdll.dll", "TpSetTimer"))
    );
    assert!(!registry.dynamic_available("ntdll.dll", "TpSetTimer"));
    registry.register(adv_abi::Api {
        module: "kernel32.dll",
        name: "SetThreadpoolTimer",
        conv: Conv::Stdcall { argc: 4 },
        env_leaf: None,
        handler: |_| Outcome::Return(0),
        level: ImplementationLevel::Implement,
    });
    // The loader binds kernel32's forwarder to ntdll; both names are callable.
    assert!(registry.is_implemented("ntdll.dll", "TpSetTimer"));
    assert!(registry.dynamic_available("ntdll.dll", "TpSetTimer"));
    assert!(registry.dynamic_available("kernel32.dll", "SetThreadpoolTimer"));
}

#[test]
fn packed_large_sdk_output_preserves_guid_bytes_and_four_byte_guest_alignment() {
    use adv_abi::types::D3DADAPTER_IDENTIFIER9;
    let mut machine = machine(Registry::default());
    // Four-byte aligned in guest memory, deliberately not eight-byte aligned.
    let output = GuestPtr::<D3DADAPTER_IDENTIFIER9>::new(DATA + 4);
    let mut value = D3DADAPTER_IDENTIFIER9 {
        DriverVersion: 0x1234_5678_9abc_def0,
        DeviceIdentifier: [0x0123_4567, 0x89ab_cdef, 0x1020_3040, 0x5060_7080],
        WHQLLevel: 0x1122_3344,
        ..D3DADAPTER_IDENTIFIER9::default()
    };
    value.Driver[511] = 17;
    value.Description[511] = 23;
    output.write(&mut machine.cpu, value).unwrap();
    assert_eq!(output.read(&machine.cpu).unwrap(), value);
    assert_eq!(machine.cpu.read_u32(DATA + 4 + 1056).unwrap(), 0x9abc_def0);
    assert_eq!(machine.cpu.read_u32(DATA + 4 + 1080).unwrap(), 0x0123_4567);
    assert_eq!(machine.cpu.read_u32(DATA + 4 + 1096).unwrap(), 0x1122_3344);
    let guid = signature::lookup("gdiplus.dll", "GdipCreateEffect").unwrap();
    assert_eq!(
        guid.stack_words(),
        5,
        "by-value GUID occupies four x86 words"
    );
}

#[test]
fn sdk_ordinals_and_names_resolve_to_the_same_patchable_entry() {
    let mut m = machine(Registry::default());
    let by_ordinal = m.resolve_import("d2d1.dll", "#1").unwrap();
    let by_name = m.resolve_import("d2d1.dll", "D2D1CreateFactory").unwrap();
    assert_eq!(by_ordinal, by_name);
    assert_eq!(
        m.sys
            .thunks
            .resolver()
            .host_fn("d2d1.dll", "#1")
            .unwrap()
            .conv,
        Conv::Stdcall { argc: 4 }
    );
    m.cpu.write(by_name, &[0xc3]).unwrap();
    assert_eq!(m.cpu.read_u8(by_ordinal).unwrap(), 0xc3);
    let mut explicit = Registry::default();
    explicit.add("d2d1.dll", "#1", Conv::Stdcall { argc: 4 }, |_| {
        Outcome::Return(19)
    });
    assert_eq!(explicit.canonical_name("d2d1.dll", "#1"), None);
    assert_eq!(explicit.canonical_name("d2d1.dll", "#65535"), None);
}

// These test adapters test ABI transport only; they are never runtime implementations.
#[win32(kernel32, name = "QueryPerformanceCounter", level = state)]
fn typed_output(
    _env: &mut Env<'_>,
    output: GuestPtr<i64>,
) -> Result<i32, adv_cpu::backend::MemFault> {
    output.write(_env.cpu, 0x1122_3344_5566_7788)?;
    Ok(1)
}

#[win32(kernel32, name = "SetFilePointerEx", level = state)]
fn typed_wide(
    _env: &mut Env<'_>,
    file: adv_abi::Handle,
    distance: i64,
    output: GuestPtr<i64>,
    method: u32,
) -> Result<i32, adv_cpu::backend::MemFault> {
    assert_eq!(file.0, 0x1234);
    assert_eq!(distance, -0x0102_0304_0506_0708);
    assert_eq!(method, 2);
    output.write(_env.cpu, distance)?;
    Ok(1)
}

#[win32(user32, name = "WindowFromPoint", level = state)]
fn typed_point(_env: &mut Env<'_>, point: adv_abi::types::POINT) -> adv_abi::Handle {
    assert_eq!(point.x, -123);
    assert_eq!(point.y, 456);
    adv_abi::Handle(0x5678)
}

fn machine(mut registry: Registry) -> Machine<UnicornCpu> {
    registry.add("fixture.dll", "done", Conv::Cdecl { argc: 0 }, |_| {
        Outcome::ExitProcess(0)
    });
    let mut machine = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    machine.cpu.map(CODE, PAGE, Prot::RWX).unwrap();
    machine.cpu.map(DATA, PAGE, Prot::RW).unwrap();
    machine
}

fn execute(machine: &mut Machine<UnicornCpu>, mut code: Asm) -> RunResult {
    let done = machine.resolve_import("fixture.dll", "done").unwrap();
    code.call(done);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("fixture", CODE, &[], 0x1_0000).unwrap();
    machine.run_virtual(1_000_000)
}

#[test]
fn typed_adapter_preserves_output_and_stdcall_stack() {
    let mut registry = Registry::default();
    registry.register(typed_output_ABI);
    assert_eq!(
        registry.level("KERNEL32.dll", "QueryPerformanceCounter"),
        Some(ImplementationLevel::State)
    );
    let mut machine = machine(registry);
    let thunk = machine
        .resolve_import("kernel32.dll", "QueryPerformanceCounter")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
    code.mov_mem_eax(DATA + 16)
        .push_imm(DATA)
        .call(thunk)
        .mov_mem_eax(DATA + 8);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 20);
    assert!(matches!(execute(&mut machine, code), RunResult::Exited(0)));
    assert_eq!(
        GuestPtr::<i64>::new(DATA).read(&machine.cpu).unwrap(),
        0x1122_3344_5566_7788
    );
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), 1);
    assert_eq!(
        machine.cpu.read_u32(DATA + 16).unwrap(),
        machine.cpu.read_u32(DATA + 20).unwrap()
    );
}

#[test]
fn metadata_uses_stack_words_for_64_bit_parameters() {
    let signature = signature::lookup("kernel32.dll", "SetFilePointerEx").unwrap();
    assert_eq!(signature.parameters.len(), 4);
    assert_eq!(signature.stack_words(), 5);
    assert_eq!(typed_wide_ABI.conv, Conv::Stdcall { argc: 5 });
    let mut registry = Registry::default();
    registry.register(typed_wide_ABI);
    let mut machine = machine(registry);
    let thunk = machine
        .resolve_import("kernel32.dll", "SetFilePointerEx")
        .unwrap();
    let distance = -0x0102_0304_0506_0708i64;
    let mut code = Asm::new(CODE);
    code.push_imm(2)
        .push_imm(DATA)
        .push_imm((distance >> 32) as u32)
        .push_imm(distance as u32)
        .push_imm(0x1234)
        .call(thunk);
    assert!(matches!(execute(&mut machine, code), RunResult::Exited(0)));
    assert_eq!(
        GuestPtr::<i64>::new(DATA).read(&machine.cpu).unwrap(),
        distance
    );
}

#[test]
fn guest_pointers_reject_null_overflow_and_protected_writes() {
    let mut machine = machine(Registry::default());
    assert!(GuestPtr::<u32>::new(0).read(&machine.cpu).is_err());
    assert!(
        GuestPtr::<u32>::new(u32::MAX - 1)
            .read(&machine.cpu)
            .is_err()
    );
    assert!(GuestPtr::<u64>::new(u32::MAX - 3).offset(1).is_err());
    machine.cpu.write_u32(DATA, 0xaabb_ccdd).unwrap();
    machine.cpu.protect(DATA, PAGE, Prot::READ).unwrap();
    assert_eq!(
        GuestPtr::<u32>::new(DATA).read(&machine.cpu).unwrap(),
        0xaabb_ccdd
    );
    assert!(
        GuestPtr::<u32>::new(DATA)
            .write(&mut machine.cpu, 0)
            .is_err()
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0xaabb_ccdd);
}

#[test]
// spec: PERS-EXPORT-5
fn strict_stub_stops_and_records_exact_caller_without_returning_success() {
    let mut machine = machine(Registry::default());
    let thunk = machine
        .resolve_import("dsound.dll", "DirectSoundCreate")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(DATA)
        .push_imm(DATA + 32)
        .push_imm(0)
        .call(thunk);
    let caller = CODE + code.bytes.len() as u32;
    let RunResult::Failed(failure) = execute(&mut machine, code) else {
        panic!("strict stub returned");
    };
    assert!(failure.message.contains("strict ABI stub"));
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 1);
    assert_eq!(diagnostics.recent_stubs[0].caller, caller);
    assert_eq!(diagnostics.recent_stubs[0].name, "DirectSoundCreate");
    assert_eq!(
        diagnostics.warnings.get(&(
            "dsound.dll!DirectSoundCreate".into(),
            "API has no implemented frontend; using a generated ABI failure stub".into(),
        )),
        Some(&1),
    );
    assert!(!diagnostics.acceptance_eligible(RunMode::Strict));
    assert!(
        machine
            .sys
            .thunks
            .iter()
            .any(|(_, t)| t.name == "DirectSoundCreate" && !t.implemented)
    );
}

#[test]
// spec: PERS-EXPORT-5
fn discovery_returns_failure_sets_thread_error_and_preserves_stdcall_stack() {
    let mut registry = Registry::default();
    registry.set_mode(RunMode::Discovery);
    let mut machine = machine(registry);
    let thunk = machine
        .resolve_import("dsound.dll", "DirectSoundCreate")
        .unwrap();
    machine.cpu.write_u32(DATA, 0xdead_beef).unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 16)
        .push_imm(DATA)
        .push_imm(DATA + 32)
        .push_imm(0)
        .call(thunk)
        .mov_mem_eax(DATA + 4);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 20);
    assert!(matches!(execute(&mut machine, code), RunResult::Exited(0)));
    assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 0x8000_4001);
    assert_eq!(
        machine.cpu.read_u32(DATA).unwrap(),
        0xdead_beef,
        "stub must not manufacture a COM object"
    );
    assert_eq!(
        machine.cpu.read_u32(DATA + 16).unwrap(),
        machine.cpu.read_u32(DATA + 20).unwrap()
    );
    let context = &machine.sys.contexts[machine.sys.current as usize];
    assert_eq!(
        machine.cpu.read_u32(context.teb + teb::LAST_ERROR).unwrap(),
        120
    );
    assert_eq!(
        machine.ext.get::<ApiDiagnostics>().gaps["dsound.dll!DirectSoundCreate"],
        1
    );
    assert_eq!(machine.ext.get::<ApiDiagnostics>().warnings.len(), 1);
}

#[test]
// spec: PERS-EXPORT-4
fn reviewed_export_returns_its_failure_contract_in_strict_mode_without_a_stub_call() {
    let mut machine = machine(Registry::default());
    let thunk = machine
        .resolve_import("dwrite.dll", "DWriteCreateFactory")
        .unwrap();
    machine.cpu.write_u32(DATA, 0xdead_beef).unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 16)
        .push_imm(DATA)
        .push_imm(DATA + 32)
        .push_imm(0)
        .call(thunk)
        .mov_mem_eax(DATA + 4);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 20);
    assert!(matches!(execute(&mut machine, code), RunResult::Exited(0)));
    assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 0x8000_4001);
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0xdead_beef);
    assert_eq!(
        machine.cpu.read_u32(DATA + 16).unwrap(),
        machine.cpu.read_u32(DATA + 20).unwrap()
    );
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert_eq!(
        diagnostics.reviewed_calls["dwrite.dll!DWriteCreateFactory"],
        1
    );
    assert!(diagnostics.acceptance_eligible(RunMode::Strict));
}

#[test]
// spec: PERS-EXPORT-5
fn discovery_derives_failures_from_return_types_and_refuses_wide_returns() {
    for (module, name, failure) in [
        ("user32.dll", "MessageBeep", Some(0)),
        ("advapi32.dll", "RegOpenKeyExA", Some(120)),
        ("ntdll.dll", "LdrLoadDll", Some(0xc000_0002)),
        ("kernel32.dll", "Sleep", Some(0)),
        ("kernel32.dll", "GetModuleHandleA", Some(0)),
        ("kernel32.dll", "WaitForSingleObject", Some(u32::MAX)),
        ("gdi32.dll", "SetTextColor", Some(u32::MAX)),
        ("winmm.dll", "waveOutOpen", Some(8)),
        ("winmm.dll", "waveOutGetNumDevs", Some(0)),
        ("ws2_32.dll", "connect", Some(u32::MAX)),
        ("ws2_32.dll", "socket", Some(u32::MAX)),
        ("ucrtbase.dll", "wcscat_s", Some(40)),
        ("kernel32.dll", "GetTickCount64", None),
        ("msvcrt.dll", "floor", None),
    ] {
        let signature = signature::lookup(module, name).unwrap();
        assert_eq!(adv_abi::stub::derived_failure(signature), failure, "{name}");
    }
    let mut registry = Registry::default();
    registry.set_mode(RunMode::Discovery);
    let mut machine = machine(registry);
    let thunk = machine
        .resolve_import("kernel32.dll", "GetTickCount")
        .unwrap();
    machine.cpu.write_u32(DATA, 0x1234).unwrap();
    let mut code = Asm::new(CODE);
    code.call(thunk).mov_mem_eax(DATA);
    assert!(matches!(execute(&mut machine, code), RunResult::Exited(0)));
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    let context = &machine.sys.contexts[machine.sys.current as usize];
    assert_eq!(
        machine.cpu.read_u32(context.teb + teb::LAST_ERROR).unwrap(),
        120
    );
    let mut registry = Registry::default();
    registry.set_mode(RunMode::Discovery);
    let mut wide = self::machine(registry);
    let thunk = wide
        .resolve_import("kernel32.dll", "GetTickCount64")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.call(thunk);
    let RunResult::Failed(failure) = execute(&mut wide, code) else {
        panic!("a 64-bit failure was guessed");
    };
    assert!(failure.message.contains("no derivable failure"));
}

#[test]
fn generated_com_slots_and_legacy_conflicts_are_visible() {
    let present = signature::lookup_com("IDirect3DDevice9", "Present").unwrap();
    assert_eq!(present.com_slot, Some(17));
    assert_eq!(present.stack_words(), 5);
    assert_eq!(
        signature::lookup_com("IDirectDraw7", "QueryInterface")
            .unwrap()
            .com_slot,
        Some(0)
    );
    let mut registry = Registry::default();
    registry.add(
        "kernel32.dll",
        "SetFilePointerEx",
        Conv::Stdcall { argc: 4 },
        |_| Outcome::Return(0),
    );
    assert_eq!(registry.signature_conflicts().len(), 1);
    assert_eq!(
        registry.signature_conflicts()[0].metadata,
        Some(Conv::Stdcall { argc: 5 })
    );
    assert!(registry.get("d3d11.dll", "D3D11CreateDevice").is_some());
    assert!(
        registry
            .exports("dwrite.dll")
            .iter()
            .any(|s| s == "DWriteCreateFactory")
    );
    assert_eq!(registry.coverage().legacy, 1);
}

#[test]
fn metadata_by_value_struct_is_a_stack_copy() {
    let mut registry = Registry::default();
    registry.register(typed_point_ABI);
    assert_eq!(typed_point_ABI.conv, Conv::Stdcall { argc: 2 });
    let mut machine = machine(registry);
    let thunk = machine
        .resolve_import("user32.dll", "WindowFromPoint")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(456)
        .push_imm(-123i32 as u32)
        .call(thunk)
        .mov_mem_eax(DATA);
    assert!(matches!(execute(&mut machine, code), RunResult::Exited(0)));
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0x5678);
}

#[test]
fn generated_flags_are_formatted_without_hiding_unknown_bits() {
    let type_name = "Windows.Win32.System.Memory.VIRTUAL_ALLOCATION_TYPE";
    assert_eq!(
        adv_abi::constants::named(type_name, "MEM_COMMIT"),
        Some(0x1000)
    );
    let text = adv_abi::constants::format(type_name, 0x1000 | 0x2000 | 1).unwrap();
    assert!(text.contains("MEM_COMMIT"));
    assert!(text.contains("MEM_RESERVE"));
    assert!(text.contains("0x1"));
    let signature = signature::lookup("kernel32.dll", "VirtualAlloc").unwrap();
    let mut machine = machine(Registry::default());
    machine.cpu.set_reg(adv_cpu::backend::Reg::Esp, DATA);
    for (i, value) in [0x1234, 0, PAGE, 0x3000, 4].into_iter().enumerate() {
        machine.cpu.write_u32(DATA + i as u32 * 4, value).unwrap();
    }
    let arguments = adv_abi::trace::format_arguments(&machine.cpu, signature);
    assert!(arguments[2].contains("MEM_COMMIT"));
    assert!(arguments[2].contains("MEM_RESERVE"));
}

#[test]
fn gdiplus_status_failure_is_nonzero_and_generated_tables_have_no_duplicates() {
    let signature = signature::lookup("gdiplus.dll", "GdiplusStartup").unwrap();
    assert_eq!(adv_abi::stub::failure_value(signature), Some(6));
    assert_eq!(signature.conv, Some(Conv::Stdcall { argc: 3 }));
    for signatures in signature::signatures().windows(2) {
        assert!(
            (signatures[0].module, signatures[0].name) < (signatures[1].module, signatures[1].name)
        );
    }
    for signature in signature::signatures() {
        if let Some(conv) = signature.conv {
            assert_eq!(u16::from(conv.argc()), signature.stack_words());
        }
    }
}

#[test]
// spec: PERS-ABI-1
fn selected_d3dx_graphics_helpers_keep_sdk_scalar_and_pointer_abi_kinds() {
    use signature::AbiKind::{F32, I32, Pointer, U32};
    for (name, kinds) in [
        ("D3DXCreateLine", &[Pointer, Pointer][..]),
        (
            "D3DXCreateSphere",
            &[Pointer, F32, U32, U32, Pointer, Pointer][..],
        ),
        (
            "D3DXCreateTextureFromFileInMemory",
            &[Pointer, Pointer, U32, Pointer][..],
        ),
        (
            "D3DXLoadMeshFromXInMemory",
            &[
                Pointer, U32, U32, Pointer, Pointer, Pointer, Pointer, Pointer, Pointer,
            ][..],
        ),
    ] {
        let signature = signature::lookup("d3dx9_43.dll", name).unwrap();
        assert_eq!(signature.return_kind, I32);
        assert_eq!(signature.return_type, "HRESULT");
        assert_eq!(
            signature.conv,
            Some(Conv::Stdcall {
                argc: kinds.len() as u8
            })
        );
        assert_eq!(signature.stack_words(), kinds.len() as u16);
        assert_eq!(
            signature
                .parameters
                .iter()
                .map(|parameter| parameter.kind)
                .collect::<Vec<_>>(),
            kinds
        );
        assert!(signature::lookup("d3dx9_29.dll", name).is_none());
    }
}

// spec: EXEC-CORE-4
#[test]
fn audio_registration_rejects_both_leaf_kinds() {
    for (module, name) in [
        ("DSOUND.DLL", "IDirectSoundBuffer8::Play"),
        ("quartz.dll", "IMediaControl::Run"),
        ("wmvcore.dll", "IWMSyncReader::GetNextSample"),
        ("winmm.dll", "waveOutWrite"),
        ("winmm.dll", "waveInStart"),
        ("winmm.dll", "midiOutShortMsg"),
        ("winmm.dll", "mciSendStringW"),
        ("winmm.dll", "mixerGetControlDetailsW"),
        ("winmm.dll", "auxSetVolume"),
        ("winmm.dll", "PlaySoundW"),
        ("winmm.dll", "sndPlaySoundA"),
    ] {
        for env_leaf in [false, true] {
            let result = std::panic::catch_unwind(|| {
                let mut registry = Registry::default();
                if env_leaf {
                    registry.add_env_leaf(
                        module,
                        name,
                        Conv::Stdcall { argc: 0 },
                        |_| Outcome::Return(0),
                        |_| Some(0),
                    );
                } else {
                    registry.add_leaf(module, name, Conv::Stdcall { argc: 0 }, |_| 0);
                }
            });
            assert!(result.is_err(), "{module}!{name}: env_leaf={env_leaf}");
        }
    }
    for name in ["timeGetTime", "timeBeginPeriod", "timeSetEvent"] {
        assert!(!adv_machine::thunk::is_audio_binding("winmm.dll", name));
    }
    assert!(!adv_machine::thunk::is_audio_binding(
        "fixture.dll",
        "waveOutWrite"
    ));
}

#[test]
// spec: PERS-EXPORT-10
fn mode_contracts_name_declared_scalar_parameters() {
    let mut registry = Registry::default();
    registry.accept("KERNEL32.dll", "SetFilePointer", "dwMoveMethod", |m| m <= 2);
    let accepts = registry
        .accepts("kernel32.dll", "SetFilePointer", "dwMoveMethod")
        .unwrap();
    assert!(accepts(2) && !accepts(3));
    assert!(
        registry
            .accepts("kernel32.dll", "SetFilePointer", "lDistanceToMove")
            .is_none()
    );
    for (name, param) in [
        ("SetFilePointer", "lpDistanceToMoveHigh"),
        ("SetFilePointer", "dwMode"),
    ] {
        let declared = std::panic::catch_unwind(|| {
            Registry::default().accept("kernel32.dll", name, param, |_| true)
        });
        assert!(
            declared.is_err(),
            "{param} is not a declared scalar parameter"
        );
    }
}

// spec: PERS-ABI-1, PERS-EXPORT-6
#[test]
fn fixed_byte_arrays_retain_extent_and_cannot_be_called_or_resized_as_pointer_cells() {
    use adv_machine::Resolver;
    let declaration = signature::lookup("msvcr90.dll", "_iob").unwrap();
    assert_eq!(declaration.data_size, Some(640));
    assert_eq!(declaration.return_kind, adv_abi::AbiKind::Aggregate);
    assert!(declaration.conv.is_none());
    assert!(!declaration.callable());
    assert!(declaration.parameters.is_empty());
    let mut registry = Registry::default();
    for size in [4, 128, 639, 641] {
        assert!(
            registry
                .register_data("msvcr90.dll", "_iob", &vec![0; size])
                .is_err()
        );
    }
    let mut bytes = vec![0; 640];
    bytes[639] = 0x5a;
    registry
        .register_data("msvcr90.dll", "_iob", &bytes)
        .unwrap();
    assert_eq!(registry.data_size("msvcr90.dll", "_iob"), Some(640));
    assert_eq!(registry.data("msvcr90.dll", "_iob"), Some(bytes.as_slice()));
    assert!(registry.dynamic_available("msvcr90.dll", "_iob"));
    assert!(registry.get("msvcr90.dll", "_iob").is_none());
    assert!(registry.set_data("msvcr90.dll", "_iob", &[0; 4]).is_err());
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            registry.add("msvcr90.dll", "_iob", Conv::Cdecl { argc: 0 }, |_| {
                Outcome::Return(0)
            });
        }))
        .is_err()
    );
}
