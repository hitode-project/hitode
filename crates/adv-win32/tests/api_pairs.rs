//! Paired exports share identities and process-owned state, including cleanup
//! after an unavailable facility and GDI save/modify/restore sequences.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg},
};
use adv_machine::{Clock, Env, Machine, Outcome, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const DATA: u32 = 0x410000;

fn machine() -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(".", "api-pairs.exe", "target/scratch/api-pairs"),
    )
    .unwrap();
    machine.spawn("API pairs", PARK_ADDR, &[], 65536).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}

fn call(machine: &mut Machine<JitCpu>, module: &str, name: &str, args: &[u32]) -> u32 {
    let entry = machine.resolve_import(module, name).unwrap();
    let index = (entry - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let function = machine.sys.thunks.get(index).f;
    machine.cpu.set_reg(Reg::Esp, DATA + 2048);
    for (i, &arg) in args.iter().enumerate() {
        machine
            .cpu
            .write_u32(DATA + 2052 + i as u32 * 4, arg)
            .unwrap();
    }
    let outcome = (function.handler)(&mut Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: index,
    });
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("{module}!{name} must return"),
    }
}

#[test]
fn missing_zw_pairs_are_discoverable_before_install_and_share_transport_identity() {
    let names = [
        "Continue",
        "CreateEvent",
        "CreateMutant",
        "CreateSemaphore",
        "CreateThreadEx",
        "DelayExecution",
        "FlushInstructionCache",
        "GetContextThread",
        "OpenEvent",
        "OpenMutant",
        "OpenSemaphore",
        "QueryAttributesFile",
        "QueryFullAttributesFile",
        "QueueApcThread",
        "RaiseException",
        "ReadVirtualMemory",
        "ReleaseMutant",
        "ReleaseSemaphore",
        "ResetEvent",
        "ResumeThread",
        "SetContextThread",
        "SetEvent",
        "SuspendThread",
        "TerminateThread",
        "TestAlert",
        "WaitForMultipleObjects",
        "WaitForSingleObject",
        "WriteVirtualMemory",
    ];
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for suffix in names {
        let (nt, zw) = (format!("Nt{suffix}"), format!("Zw{suffix}"));
        assert!(registry.dynamic_available("ntdll.dll", &zw), "{zw}");
        let original = registry.get("ntdll.dll", &nt).unwrap();
        let alias = registry.get("ntdll.dll", &zw).unwrap();
        assert_eq!(alias.conv, original.conv);
        assert_eq!(alias.data, original.data);
        assert!(std::ptr::fn_addr_eq(alias.handler, original.handler));
    }
    let mut machine = machine();
    for suffix in names {
        assert_eq!(
            machine
                .resolve_import("ntdll.dll", &format!("Nt{suffix}"))
                .unwrap(),
            machine
                .resolve_import("ntdll.dll", &format!("Zw{suffix}"))
                .unwrap()
        );
    }
    let event = machine.sys.objects.insert(adv_machine::Object::Event {
        manual_reset: true,
        signaled: false,
    });
    assert_eq!(
        call(&mut machine, "ntdll.dll", "ZwSetEvent", &[event, DATA]),
        0
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    assert_eq!(
        call(&mut machine, "ntdll.dll", "NtResetEvent", &[event, DATA]),
        0
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 1);
}

#[test]
fn absent_caret_cleanup_returns_without_adding_an_owner_or_a_gap() {
    let mut machine = machine();
    let window = machine.ext.get::<Win32>().user.desktop;
    let count = machine.sys.objects.object_count();
    assert_eq!(
        call(
            &mut machine,
            "user32.dll",
            "CreateCaret",
            &[window, 0, 1, 1]
        ),
        0
    );
    for (name, args) in [
        ("DestroyCaret", vec![]),
        ("ShowCaret", vec![0]),
        ("HideCaret", vec![window]),
        ("SetCaretPos", vec![3, 4]),
    ] {
        assert_eq!(call(&mut machine, "user32.dll", name, &args), 0, "{name}");
        assert_eq!(call(&mut machine, "kernel32.dll", "GetLastError", &[]), 5);
    }
    assert_eq!(
        call(&mut machine, "user32.dll", "HideCaret", &[0x12345678]),
        0
    );
    assert_eq!(
        call(&mut machine, "kernel32.dll", "GetLastError", &[]),
        1400
    );
    machine.cpu.write(DATA, &[0xaa; 8]).unwrap();
    assert_eq!(call(&mut machine, "user32.dll", "GetCaretPos", &[DATA]), 1);
    let mut point = [1; 8];
    machine.cpu.read(DATA, &mut point).unwrap();
    assert_eq!(point, [0; 8]);
    assert_eq!(call(&mut machine, "user32.dll", "GetCaretPos", &[0]), 0);
    assert_eq!(machine.sys.objects.object_count(), count);
    assert_eq!(
        machine.ext.get::<adv_abi::ApiDiagnostics>().warnings.len(),
        1
    );
}

#[test]
fn gdi_queries_observe_the_same_saved_dc_state() {
    let mut machine = machine();
    let dc = call(&mut machine, "gdi32.dll", "CreateCompatibleDC", &[0]);
    assert_ne!(dc, 0);
    assert_eq!(call(&mut machine, "gdi32.dll", "GetTextColor", &[dc]), 0);
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetBkColor", &[dc]),
        0xffffff
    );
    assert_eq!(call(&mut machine, "gdi32.dll", "GetBkMode", &[dc]), 2);
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetStretchBltMode", &[dc]),
        1
    );
    let saved = call(&mut machine, "gdi32.dll", "SaveDC", &[dc]);
    for (name, args) in [
        ("SetTextColor", vec![dc, 0x123456]),
        ("SetBkColor", vec![dc, 0x654321]),
        ("SetBkMode", vec![dc, 1]),
        ("SetViewportOrgEx", vec![dc, (-7i32) as u32, 9, 0]),
    ] {
        call(&mut machine, "gdi32.dll", name, &args);
    }
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetTextColor", &[dc]),
        0x123456
    );
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetBkColor", &[dc]),
        0x654321
    );
    assert_eq!(call(&mut machine, "gdi32.dll", "GetBkMode", &[dc]), 1);
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetViewportOrgEx", &[dc, DATA]),
        1
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), (-7i32) as u32);
    assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 9);
    assert_eq!(
        call(&mut machine, "gdi32.dll", "RestoreDC", &[dc, saved]),
        1
    );
    assert_eq!(call(&mut machine, "gdi32.dll", "GetTextColor", &[dc]), 0);
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetBkColor", &[dc]),
        0xffffff
    );
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetViewportOrgEx", &[dc, DATA]),
        1
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    for name in ["GetTextColor", "GetBkColor"] {
        assert_eq!(call(&mut machine, "gdi32.dll", name, &[0]), u32::MAX);
    }
    for name in ["GetBkMode", "GetStretchBltMode"] {
        assert_eq!(call(&mut machine, "gdi32.dll", name, &[0]), 0);
    }
    machine.cpu.write(DATA, &[0xaa; 8]).unwrap();
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetViewportOrgEx", &[0, DATA]),
        0
    );
    let mut unchanged = [0; 8];
    machine.cpu.read(DATA, &mut unchanged).unwrap();
    assert_eq!(unchanged, [0xaa; 8]);
}

#[cfg(windows)]
#[test]
fn no_caret_contract_matches_the_native_thread_queue() {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn DestroyCaret() -> i32;
        fn ShowCaret(window: *mut core::ffi::c_void) -> i32;
        fn HideCaret(window: *mut core::ffi::c_void) -> i32;
        fn SetCaretPos(x: i32, y: i32) -> i32;
        fn GetCaretPos(point: *mut [i32; 2]) -> i32;
    }
    let mut point = [123, 456];
    unsafe {
        assert_eq!(DestroyCaret(), 0);
        assert_eq!(ShowCaret(core::ptr::null_mut()), 0);
        assert_eq!(HideCaret(core::ptr::null_mut()), 0);
        assert_eq!(SetCaretPos(7, 9), 0);
        assert_ne!(GetCaretPos(&mut point), 0);
    }
    assert_eq!(point, [0, 0]);
}
