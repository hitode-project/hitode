use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_machine::{Clock, Machine, Outcome, layout::PARK_ADDR};

pub(super) fn fixture() -> Machine<JitCpu> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/scratch/keymap-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    machine.spawn("keymap", PARK_ADDR, &[], 65536).unwrap();
    machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
    machine
}
pub(super) fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}

#[test]
fn map_virtual_key_uses_guest_profile_and_keeps_unknown_keys_distinct_from_missing_capability() {
    let mut machine = fixture();
    let stack = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for name in ["MapVirtualKeyA", "MapVirtualKeyW"] {
        assert!(registry.dynamic_available("user32.dll", name));
        let api = registry.get("user32.dll", name).unwrap();
        assert_eq!(api.conv, adv_abi::Conv::Stdcall { argc: 2 });
        machine.cpu.write_u32(stack + 4, 0x0a).unwrap();
        machine.cpu.write_u32(stack + 8, 0).unwrap();
        machine.cpu.set_reg(Reg::Esp, stack);
        crate::util::set_last_error(&mut env(&mut machine), 0x12345678);
        assert!(matches!(
            (api.handler)(&mut env(&mut machine)),
            Outcome::Return(0)
        ));
        assert_eq!(crate::util::last_error(&env(&mut machine)), 0x12345678);
    }
    for layout in [0x0411_0411, 0x0804_0804, 0x0409_0409] {
        user_mut(&mut env(&mut machine)).keyboard_layout = layout;
        for (code, kind, expected) in [
            (0x41, 0, 0x1e),
            (0x10, 0, 0x2a),
            (0xa3, 4, 0xe01d),
            (0xe01d, 1, 0x11),
            (0xe01d, 3, 0xa3),
            (0xe11d, 3, 0x13),
            (0x13, 0, 0),
            (0x24, 4, 0x47),
            (0x1e, 3, 0x41),
            (0x41, 2, 0x41),
            (0xbd, 2, 0x2d),
            (0xffffffff, 0, 0),
        ] {
            assert_eq!(
                map_virtual_key_a(&mut env(&mut machine), code, kind),
                expected
            );
            assert_eq!(
                map_virtual_key_w(&mut env(&mut machine), code, kind),
                expected
            );
        }
        assert_eq!(
            map_virtual_key_w(&mut env(&mut machine), 0x1a, 4),
            if layout == 0x0411_0411 { 0xe0f1 } else { 0 }
        );
    }
    assert!(
        machine
            .ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .is_empty()
    );
    assert_eq!(map_virtual_key_a(&mut env(&mut machine), 0x41, 99), 0);
    assert_eq!(crate::util::last_error(&env(&mut machine)), 50);
    user_mut(&mut env(&mut machine)).keyboard_layout = 0x0407_0407;
    assert_eq!(map_virtual_key_w(&mut env(&mut machine), 0x41, 0), 0);
    assert_eq!(crate::util::last_error(&env(&mut machine)), 50);
    assert_eq!(
        machine.ext.get::<adv_abi::ApiDiagnostics>().warnings.len(),
        2
    );
}

#[cfg(windows)]
#[test]
fn map_virtual_key_matches_explicit_native_enhanced_layouts_without_activation() {
    use core::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn LoadKeyboardLayoutW(name: *const u16, flags: u32) -> *mut c_void;
        fn GetKeyboardLayout(thread: u32) -> *mut c_void;
        fn GetKeyboardType(kind: i32) -> i32;
        fn GetKeyboardLayoutList(count: i32, list: *mut *mut c_void) -> i32;
        fn UnloadKeyboardLayout(layout: *mut c_void) -> i32;
        fn MapVirtualKeyExA(code: u32, kind: u32, layout: *mut c_void) -> u32;
        fn MapVirtualKeyExW(code: u32, kind: u32, layout: *mut c_void) -> u32;
    }
    let mut machine = fixture();
    struct NativeLayout {
        handle: *mut c_void,
        unload: bool,
    }
    impl Drop for NativeLayout {
        fn drop(&mut self) {
            // SAFETY: only layouts loaded by this test are retired here.
            if self.unload {
                unsafe {
                    UnloadKeyboardLayout(self.handle);
                }
            }
        }
    }
    // SAFETY: valid layout names and output buffers; this test owns no HWND and
    // never sets ACTIVATE, REORDER or SUBSTITUTE_OK. Only new loads are unloaded.
    unsafe {
        if GetKeyboardType(0) != 4 || GetKeyboardType(1) != 0 {
            eprintln!("native keyboard hardware differs from the guest enhanced profile");
            return;
        }
        let initial = GetKeyboardLayout(0);
        let count = GetKeyboardLayoutList(0, std::ptr::null_mut());
        let mut existing = vec![std::ptr::null_mut(); count as usize];
        assert_eq!(GetKeyboardLayoutList(count, existing.as_mut_ptr()), count);
        for (name, profile) in [
            ("00000409", 0x0409_0409),
            ("00000411", 0x0411_0411),
            ("00000804", 0x0804_0804),
        ] {
            let name: Vec<_> = name.encode_utf16().chain([0]).collect();
            let layout = LoadKeyboardLayoutW(name.as_ptr(), 0x80); // KLF_NOTELLSHELL.
            assert!(!layout.is_null());
            let _loaded = NativeLayout {
                handle: layout,
                unload: !existing.contains(&layout),
            };
            assert_eq!(GetKeyboardLayout(0), initial);
            user_mut(&mut env(&mut machine)).keyboard_layout = profile;
            for kind in 0..=4 {
                let prefixes: &[u32] = if matches!(kind, 1 | 3) {
                    &[0, 0xe000, 0xe100]
                } else {
                    &[0]
                };
                for &prefix in prefixes {
                    for code in 0..=255 {
                        let code = prefix | code;
                        assert_eq!(
                            map_virtual_key_a(&mut env(&mut machine), code, kind),
                            MapVirtualKeyExA(code, kind, layout),
                            "profile={profile:#x} code={code:#x} kind={kind}"
                        );
                        assert_eq!(
                            map_virtual_key_w(&mut env(&mut machine), code, kind),
                            MapVirtualKeyExW(code, kind, layout),
                            "profile={profile:#x} code={code:#x} kind={kind}"
                        );
                    }
                }
            }
        }
        assert_eq!(GetKeyboardLayout(0), initial);
    }
}
