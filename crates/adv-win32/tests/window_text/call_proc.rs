//! Saved shared USER procedures keep their identity across an A/W API boundary.
use super::*;
use adv_win32::user32::window::GWL_WNDPROC;

fn shared_saved<B: TestCpu>(cpu: B, stock_class: &str) {
    let mut t = Fixture::new(cpu, 936);
    let edit = t.text(stock_class, true);
    let stock_window = t.call(
        "CreateWindowExW",
        &[
            I(0),
            I(edit),
            I(0),
            I(WS_POPUP),
            I(0),
            I(0),
            I(80),
            I(20),
            I(0),
            I(0),
            I(CODE),
            I(0),
        ],
    );
    let saved = t.call("GetWindowLongW", &[M(stock_window), I(GWL_WNDPROC as u32)]);
    let copied = t.alloc(64);
    let replacement = t.callback(true, copied, None);
    let forwarded_creation = t.alloc(4);
    let setter = t.function("SetWindowLongW");
    let call_a = t.function("CallWindowProcA");
    let default_w = t.function("DefWindowProcW");
    let mut initial = Asm::new(t.procedure);
    on_message(&mut initial, wm::NCCREATE, |code| {
        // Reproduce the observed sequence: captured W window/CREATESTRUCTW,
        // replace its current procedure, then CallA the saved shared stock proc.
        invoke(code, setter, &[S(4), I(GWL_WNDPROC as u32), I(replacement)]);
        invoke(code, call_a, &[M(saved), S(4), S(8), S(12), S(16)]);
        code.mov_mem_eax(forwarded_creation).ret_n(16);
    });
    initial.jmp(default_w);
    let initial_proc = t.procedure;
    t.machine.cpu.write(initial_proc, &initial.bytes).unwrap();
    t.machine.cpu.region(initial_proc, initial.bytes.len());
    t.procedure = initial.here().next_multiple_of(16);
    let hwnd = t.window(true, initial_proc);
    let current = t.call("GetWindowLongW", &[M(hwnd), I(GWL_WNDPROC as u32)]);
    t.set_error(0x12345678);
    let set_font = t.call(
        "CallWindowProcA",
        &[M(saved), M(hwnd), I(wm::SETFONT), I(0x76543210), I(0)],
    );
    let get_font = t.call(
        "CallWindowProcA",
        &[M(saved), M(hwnd), I(wm::GETFONT), I(0), I(0)],
    );
    let numeric_error = t.error();
    let text = t.text("中A", false);
    let output = t.alloc(32);
    let set_text = t.call(
        "CallWindowProcA",
        &[M(saved), M(hwnd), I(wm::SETTEXT), I(0), I(text)],
    );
    let length = t.call(
        "CallWindowProcA",
        &[M(saved), M(hwnd), I(wm::GETTEXTLENGTH), I(0), I(0)],
    );
    let get_text = t.call(
        "CallWindowProcA",
        &[M(saved), M(hwnd), I(wm::GETTEXT), I(32), I(output)],
    );
    let destroy = t.call(
        "CallWindowProcA",
        &[M(saved), M(hwnd), I(wm::DESTROY), I(0), I(0)],
    );
    let nc_destroy = t.call(
        "CallWindowProcA",
        &[M(saved), M(hwnd), I(wm::NCDESTROY), I(0), I(0)],
    );
    let metadata_error = t.error();
    // Character conversion and a known opposite guest procedure remain guarded.
    let character = t.call(
        "CallWindowProcA",
        &[M(saved), M(hwnd), I(wm::CHAR), I(0xa082), I(0)],
    );
    let character_error = t.error();
    let custom = t.call(
        "CallWindowProcA",
        &[I(replacement), M(hwnd), I(wm::NCCREATE), I(0), I(0)],
    );
    let custom_error = t.error();
    t.run();
    let handle = t.value(hwnd);
    assert_ne!(
        handle, 0,
        "cross-ABI saved Edit NCCREATE must accept creation"
    );
    assert_eq!(t.value(forwarded_creation), 1);
    assert_eq!(t.value(current), replacement);
    assert_eq!(t.value(set_font), 0);
    assert_eq!(t.value(get_font), 0x76543210);
    assert_eq!(t.value(numeric_error), 0x12345678);
    assert_eq!(
        (t.value(set_text), t.value(length), t.value(get_text)),
        (1, 3, 3)
    );
    assert_eq!(t.read(output, 4), [0xd6, 0xd0, b'A', 0]);
    assert_eq!((t.value(destroy), t.value(nc_destroy)), (0, 0));
    assert_eq!(t.value(metadata_error), 0x12345678);
    assert_eq!((t.value(character), t.value(character_error)), (0, 50));
    assert_eq!((t.value(custom), t.value(custom_error)), (0, 50));
    for message in [
        wm::NCCREATE,
        wm::SETFONT,
        wm::GETFONT,
        wm::SETTEXT,
        wm::GETTEXT,
        wm::GETTEXTLENGTH,
        wm::DESTROY,
        wm::NCDESTROY,
    ] {
        assert!(
            t.rows(handle, message).is_empty(),
            "saved call reached current proc: {message:#x}"
        );
    }
    assert_eq!(
        t.machine
            .ext
            .get::<Win32>()
            .user
            .windows
            .get(&t.machine.sys.objects, &handle)
            .unwrap()
            .text,
        "中A"
    );
    assert_eq!(t.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
}

#[test]
fn saved_stock_cross_abi_creation_numeric_and_text_bypass_replaced_current_procedure() {
    for stock_class in ["Edit", "Button"] {
        shared_saved(UnicornCpu::new(PARK_ADDR).unwrap(), stock_class);
        shared_saved(JitCpu::new(PARK_ADDR, false).unwrap(), stock_class);
    }
}
