use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt},
};
use adv_machine::{Clock, Machine, ObjectLimits, Objects};
fn fixture(references: u32) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    m.sys.objects = Objects::with_limits(ObjectLimits {
        references,
        ..Default::default()
    });
    crate::Win32::install(
        &mut m,
        crate::Win32Config::new(
            ".",
            "clipboard-tests.exe",
            "target/clipboard-contract-tests",
        ),
    )
    .unwrap();
    m.spawn("clipboard", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    m
}
fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn returned(value: Outcome) -> u32 {
    match value {
        Outcome::Return(n) => n,
        _ => panic!("expected immediate return"),
    }
}
fn owner(m: &Machine<JitCpu>) -> u32 {
    m.ext.get::<Win32>().user.desktop
}
#[test]
fn same_memory_multiple_formats_and_replacements_retire_once_and_never_free_borrowers() {
    let mut m = fixture(65536);
    let hwnd = owner(&m);
    assert_eq!(open_clipboard(&mut env(&mut m), Handle(hwnd)), 1);
    assert_eq!(returned(empty_clipboard(&mut env(&mut m))), 1);
    let id = allocate_bytes(&mut env(&mut m), b"opaque\0").unwrap();
    let h = m.sys.objects.owned(id).unwrap().handle;
    let f1 = register_name(&mut env(&mut m), "Rtf".encode_utf16().collect());
    assert_eq!(
        register_name(&mut env(&mut m), "rTF".encode_utf16().collect()),
        f1
    );
    let f2 = register_name(&mut env(&mut m), "Other".encode_utf16().collect());
    assert_eq!(
        set_clipboard_data(&mut env(&mut m), f1, Handle(h)),
        Handle(h)
    );
    assert_eq!(
        set_clipboard_data(&mut env(&mut m), f2, Handle(h)),
        Handle(h)
    );
    assert_eq!(
        set_clipboard_data(&mut env(&mut m), f1, Handle(h)),
        Handle(h)
    );
    assert_eq!(m.sys.objects.strong_count(id), Some(3));
    {
        let (heaps, globals) = m.ext.get2_mut::<Heaps, Globals>();
        assert_eq!(
            globals.free(
                &mut m.sys.objects,
                heaps,
                &mut m.cpu,
                &mut m.sys.vm,
                h,
                false
            ),
            Err(adv_os::global::FreeError::Owned)
        );
    }
    let replacement = allocate_bytes(&mut env(&mut m), b"new\0").unwrap();
    let r = m.sys.objects.owned(replacement).unwrap().handle;
    assert_eq!(
        set_clipboard_data(&mut env(&mut m), f1, Handle(r)),
        Handle(r)
    );
    assert_eq!(m.sys.objects.strong_count(id), Some(2));
    // Avoid a guest callback here; the PE test exercises real owner callbacks.
    m.ext.get_mut::<Win32>().user.clipboard.owner = 0;
    assert_eq!(returned(empty_clipboard(&mut env(&mut m))), 1);
    assert!(m.sys.objects.owned(id).is_none());
    assert!(m.sys.objects.owned(replacement).is_none());
    assert_eq!(m.ext.get::<Globals>().from_pointer(&m.sys.objects, 0), None);
}
#[test]
fn reference_exhaustion_delayed_render_and_foreign_thread_preserve_the_live_state() {
    let mut m = fixture(1);
    let hwnd = owner(&m);
    assert_eq!(open_clipboard(&mut env(&mut m), Handle(hwnd)), 1);
    assert_eq!(returned(empty_clipboard(&mut env(&mut m))), 1);
    let id = allocate_bytes(&mut env(&mut m), b"x\0").unwrap();
    let h = m.sys.objects.owned(id).unwrap().handle;
    assert_eq!(
        set_clipboard_data(&mut env(&mut m), TEXT, Handle(h)),
        Handle(0)
    );
    assert_eq!(
        set_clipboard_data(&mut env(&mut m), 0x300, Handle(h)),
        Handle(0)
    );
    assert_eq!(
        set_clipboard_data(&mut env(&mut m), 0x200, Handle(h)),
        Handle(0)
    );
    assert!(!m.sys.objects.owned(id).unwrap().clipboard);
    assert!(m.ext.get::<Win32>().user.clipboard.data.is_empty());
    assert_eq!(
        set_clipboard_data(&mut env(&mut m), TEXT, Handle(0)),
        Handle(0)
    );
    assert_eq!(is_clipboard_format_available(&mut env(&mut m), TEXT), 0);
    let current = m.sys.current;
    m.spawn("foreign", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    m.sys.current = m.sys.contexts.last().unwrap().id;
    assert_eq!(open_clipboard(&mut env(&mut m), Handle(hwnd)), 0);
    assert_eq!(get_clipboard_data(&mut env(&mut m), TEXT), Handle(0));
    m.sys.current = current;
    assert_eq!(returned(close_clipboard(&mut env(&mut m))), 1);
    let (heaps, globals) = m.ext.get2_mut::<Heaps, Globals>();
    globals
        .free(
            &mut m.sys.objects,
            heaps,
            &mut m.cpu,
            &mut m.sys.vm,
            h,
            false,
        )
        .unwrap();
}
#[test]
fn synthesized_dbcs_text_is_real_and_invalid_or_unwritable_queries_have_no_partial_writes() {
    let mut m = fixture(65536);
    let hwnd = owner(&m);
    open_clipboard(&mut env(&mut m), Handle(hwnd));
    empty_clipboard(&mut env(&mut m));
    let id = allocate_bytes(&mut env(&mut m), &[0x82, 0xa0, b'X', 0]).unwrap();
    let h = m.sys.objects.owned(id).unwrap().handle;
    set_clipboard_data(&mut env(&mut m), TEXT, Handle(h));
    assert_eq!(returned(close_clipboard(&mut env(&mut m))), 1);
    assert_eq!(count_clipboard_formats(&mut env(&mut m)), 4);
    open_clipboard(&mut env(&mut m), Handle(0));
    let wide = get_clipboard_data(&mut env(&mut m), UNICODE);
    let pointer = m
        .ext
        .get::<Globals>()
        .get(&m.sys.objects, wide.0)
        .unwrap()
        .pointer;
    let mut bytes = [0; 6];
    m.cpu.read(pointer, &mut bytes).unwrap();
    assert_eq!(bytes, [0x42, 0x30, 0x58, 0, 0, 0]);
    let again = get_clipboard_data(&mut env(&mut m), UNICODE);
    assert_eq!(again, wide);
    let name = "format\u{3042}".encode_utf16().collect();
    let format = register_name(&mut env(&mut m), name);
    let out = m
        .sys
        .vm
        .alloc(&mut m.cpu, 4096, Prot::READ | Prot::WRITE)
        .unwrap();
    m.cpu.write(out, &[0x77; 16]).unwrap();
    m.cpu.protect(out, 4096, Prot::READ).unwrap();
    assert_eq!(format_name(&mut env(&mut m), format, out, 8, true), 0);
    let mut bytes = [0; 16];
    m.cpu.read(out, &mut bytes).unwrap();
    assert_eq!(bytes, [0x77; 16]);
    assert_eq!(
        format_name(&mut env(&mut m), format, u32::MAX - 1, 8, true),
        0
    );
    assert_eq!(enum_clipboard_formats(&mut env(&mut m), 0xdead), 0);
    assert_eq!(returned(close_clipboard(&mut env(&mut m))), 1);
    assert_eq!(open_clipboard(&mut env(&mut m), Handle(hwnd)), 1);
    let replacement = allocate_bytes(&mut env(&mut m), b"Y\0").unwrap();
    let replacement_handle = m.sys.objects.owned(replacement).unwrap().handle;
    set_clipboard_data(&mut env(&mut m), TEXT, Handle(replacement_handle));
    assert!(
        m.ext.get::<Globals>().get(&m.sys.objects, wide.0).is_none(),
        "old synthesized data must retire when its source changes"
    );
    let replaced = get_clipboard_data(&mut env(&mut m), UNICODE);
    let pointer = m
        .ext
        .get::<Globals>()
        .get(&m.sys.objects, replaced.0)
        .unwrap()
        .pointer;
    assert_eq!(m.cpu.read_u32(pointer).unwrap(), u32::from(b'Y'));
    let malformed = allocate_bytes(&mut env(&mut m), &[b'X'; 16]).unwrap();
    let malformed_handle = m.sys.objects.owned(malformed).unwrap().handle;
    set_clipboard_data(&mut env(&mut m), TEXT, Handle(malformed_handle));
    assert_eq!(get_clipboard_data(&mut env(&mut m), UNICODE), Handle(0));
    assert!(m.sys.objects.owned(malformed).is_some());
    m.ext.get_mut::<Win32>().user.keyboard_layout = 0x0409_0409;
    assert_eq!(
        set_clipboard_data(&mut env(&mut m), TEXT, Handle(replacement_handle)),
        Handle(0)
    );
    // The replaced source was system-owned and has already retired, so use a
    // new live source for the profile capability check.
    let us = allocate_bytes(&mut env(&mut m), b"US\0").unwrap();
    let us_handle = m.sys.objects.owned(us).unwrap().handle;
    set_clipboard_data(&mut env(&mut m), TEXT, Handle(us_handle));
    assert_eq!(is_clipboard_format_available(&mut env(&mut m), OEM), 0);
    assert_eq!(get_clipboard_data(&mut env(&mut m), OEM), Handle(0));
}
#[test]
fn window_and_null_window_thread_cleanup_release_the_lock_and_viewer_links_but_keep_data() {
    let mut m = fixture(65536);
    let hwnd = owner(&m);
    open_clipboard(&mut env(&mut m), Handle(hwnd));
    empty_clipboard(&mut env(&mut m));
    let id = allocate_bytes(&mut env(&mut m), b"keep\0").unwrap();
    let h = m.sys.objects.owned(id).unwrap().handle;
    set_clipboard_data(&mut env(&mut m), TEXT, Handle(h));
    m.ext
        .get_mut::<Win32>()
        .user
        .clipboard
        .viewers
        .push(Viewer { window: hwnd });
    assert_eq!(
        returned(remove_windows(&mut env(&mut m), &[hwnd], |_| {
            Outcome::Return(9)
        })),
        9
    );
    assert_eq!(get_clipboard_owner(&mut env(&mut m)), Handle(0));
    assert_eq!(get_clipboard_viewer(&mut env(&mut m)), Handle(0));
    assert!(m.sys.objects.owned(id).unwrap().clipboard);
    assert_eq!(open_clipboard(&mut env(&mut m), Handle(0)), 1);
    assert_eq!(
        returned(exit_thread(&mut env(&mut m), |_| Outcome::Return(7))),
        7
    );
    assert!(m.ext.get::<Win32>().user.clipboard.open.is_none());
}
