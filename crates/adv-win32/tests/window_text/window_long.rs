//! Reuse the authored guest/ASM fixture for real subclass continuations.
use super::*;
use adv_win32::user32::window::{GWL_HWNDPARENT, GWL_USERDATA, GWL_WNDPROC};

fn store_esp(code: &mut Asm, pointer: u32) {
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&pointer.to_le_bytes());
}
fn install<B: TestCpu>(test: &mut Fixture<B>, code: Asm) -> u32 {
    let start = test.procedure;
    assert!(code.here() < CODE + 0x10000);
    test.machine.cpu.write(start, &code.bytes).unwrap();
    test.machine.cpu.region(start, code.bytes.len());
    test.procedure = code.here().next_multiple_of(16);
    start
}
fn window<B: TestCpu>(test: &mut Fixture<B>, wide: bool, procedure: u32) -> u32 {
    let name = test.text(&format!("Long{procedure:x}"), wide);
    let fields = [0, procedure, 0, 8, CODE, 0, 0, 0, 0, name];
    let class = test.bytes(
        &fields
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    test.call(
        if wide {
            "RegisterClassW"
        } else {
            "RegisterClassA"
        },
        &[I(class)],
    );
    test.call(
        if wide {
            "CreateWindowExW"
        } else {
            "CreateWindowExA"
        },
        &[
            I(0),
            I(name),
            I(0),
            I(WS_POPUP),
            I(0),
            I(0),
            I(100),
            I(100),
            I(0),
            I(0),
            I(CODE),
            I(0),
        ],
    )
}

fn subclass<B: TestCpu>(cpu: B) {
    let mut t = Fixture::new(cpu, 936);
    for (name, argc) in [
        ("GetWindowLongW", 2),
        ("SetWindowLongW", 3),
        ("CallWindowProcW", 5),
    ] {
        assert!(
            t.machine
                .sys
                .thunks
                .resolver()
                .implemented("user32.dll", name)
        );
        let registry = t.machine.sys.thunks.resolver();
        let binding = registry.host_fn("user32.dll", name).unwrap();
        assert_eq!(binding.conv, Conv::Stdcall { argc });
    }
    let copied = t.alloc(64);
    let copied_a = t.alloc(64);
    let old = t.alloc(4);
    let current = t.alloc(4);
    let returned = t.callback(true, copied, None);
    let call_old = t.function("CallWindowProcW");
    let mut forward = Asm::new(t.procedure);
    invoke(&mut forward, call_old, &[M(old), S(4), S(8), S(12), S(16)]);
    forward.ret_n(16);
    let replacement = install(&mut t, forward);
    let setter = t.function("SetWindowLongW");
    let getter = t.function("GetWindowLongW");
    let mut initial = Asm::new(t.procedure);
    on_message(&mut initial, wm::NCCREATE, |code| {
        invoke(code, setter, &[S(4), I(GWL_WNDPROC as u32), I(replacement)]);
        code.mov_mem_eax(old);
        invoke(code, getter, &[S(4), I(GWL_WNDPROC as u32)]);
        code.mov_mem_eax(current);
        value(code, 1);
        code.ret_n(16);
    });
    initial.jmp(returned);
    let initial = install(&mut t, initial);
    let hwnd = window(&mut t, true, initial);
    let ansi_proc = t.callback(false, copied_a, None);
    let ansi_hwnd = window(&mut t, false, ansi_proc);
    let text = t.text("中あ", true);
    let before = t.alloc(4);
    let after = t.alloc(4);
    store_esp(&mut t.code, before);
    let set = t.call("SendMessageW", &[M(hwnd), I(wm::SETTEXT), I(0), I(text)]);
    store_esp(&mut t.code, after);
    let saved = t.call(
        "CallWindowProcW",
        &[M(old), M(hwnd), I(0x8033), I(0x12345678), I(0x87654321)],
    );
    // Callback identity/encoding is independent of the current HWND's procedure.
    let arbitrary = t.callback(true, t.data, None);
    let invalid = t.call(
        "CallWindowProcW",
        &[
            I(arbitrary),
            I(0x12345678),
            I(0x8033),
            I(0x89abcdef),
            I(0xfedcba98),
        ],
    );
    let on_ansi = t.call(
        "CallWindowProcW",
        &[I(arbitrary), M(ansi_hwnd), I(0x8033), I(0), I(0)],
    );
    let restored = t.call("SetWindowLongW", &[M(hwnd), I(GWL_WNDPROC as u32), M(old)]);
    let get_restored = t.call("GetWindowLongW", &[M(hwnd), I(GWL_WNDPROC as u32)]);
    t.call("DestroyWindow", &[M(hwnd)]);
    // The captured old callback remains callable after its window is destroyed.
    let retired = t.call("CallWindowProcW", &[M(old), M(hwnd), I(0x8033), I(7), I(9)]);
    t.run();
    assert_eq!((t.value(old), t.value(current)), (initial, replacement));
    assert_eq!(
        (t.value(restored), t.value(get_restored)),
        (replacement, initial)
    );
    assert_eq!(t.value(set), 0x55);
    assert_eq!(t.value(before), t.value(after));
    assert_eq!(t.read(copied, utf16("中あ").len()), utf16("中あ"));
    for p in [saved, invalid, on_ansi, retired] {
        assert_eq!(t.value(p), 0xfedcba98);
    }
    assert_eq!(
        t.rows(0x12345678, 0x8033),
        [[0x12345678, 0x8033, 0x89abcdef, 0xfedcba98]]
    );
    assert_eq!(t.rows(t.value(hwnd), wm::SETTEXT).len(), 1);
    assert_eq!(t.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
}

#[test]
fn subclass_w_during_nccreate_calls_saved_target_with_real_text_and_stack_results() {
    subclass(UnicornCpu::new(PARK_ADDR).unwrap());
    subclass(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn errors<B: TestCpu>(cpu: B) {
    let mut t = Fixture::new(cpu, 936);
    let aw = t.alloc(64);
    let aa = t.alloc(64);
    let wproc = t.callback(true, aw, None);
    let aproc = t.callback(false, aa, None);
    let w = window(&mut t, true, wproc);
    let a = window(&mut t, false, aproc);
    let data = t.call(
        "SetWindowLongW",
        &[M(w), I(GWL_USERDATA as u32), I(0x87654321)],
    );
    let de = t.error();
    let extra = t.call("SetWindowLongW", &[M(w), I(4), I(0xfedcba98)]);
    let read_a = t.call("GetWindowLongA", &[M(w), I(GWL_USERDATA as u32)]);
    let read_extra = t.call("GetWindowLongW", &[M(w), I(4)]);
    t.set_error(0x12345678);
    let zero = t.call("GetWindowLongW", &[M(a), I(GWL_USERDATA as u32)]);
    let ze = t.error();
    let mut failures = Vec::new();
    for (hwnd, index, value, error) in [
        (I(0x12345678), GWL_USERDATA, 7, 1400),
        (M(w), -123, 7, 1413),
        (M(w), 5, 7, 1413),
        (M(w), GWL_WNDPROC, 0, 87),
        (M(a), GWL_WNDPROC, wproc, 50),
        (M(w), GWL_WNDPROC, aproc, 50),
        (M(w), GWL_HWNDPARENT, 0x12345678, 50),
    ] {
        let result = t.call("SetWindowLongW", &[hwnd, I(index as u32), I(value)]);
        let last = t.error();
        failures.push((result, last, error));
    }
    let cross_get = t.call("GetWindowLongW", &[M(a), I(GWL_WNDPROC as u32)]);
    let cge = t.error();
    let cross_a = t.call("SetWindowLongA", &[M(w), I(GWL_WNDPROC as u32), I(aproc)]);
    let cae = t.error();
    let parent_a = t.call(
        "SetWindowLongA",
        &[M(a), I(GWL_HWNDPARENT as u32), I(0x12345678)],
    );
    let pae = t.error();
    let cross_call = t.call("CallWindowProcW", &[I(aproc), M(a), I(0x8033), I(0), I(0)]);
    let cce = t.error();
    let final_w = t.call("GetWindowLongW", &[M(w), I(GWL_WNDPROC as u32)]);
    let final_a = t.call("GetWindowLongA", &[M(a), I(GWL_WNDPROC as u32)]);
    // A and W stock windows intentionally share a host procedure address.
    let stock = t.text("Static", false);
    let stock_a = t.call(
        "CreateWindowExA",
        &[
            I(0),
            I(stock),
            I(0),
            I(WS_POPUP),
            I(0),
            I(0),
            I(10),
            I(10),
            I(0),
            I(0),
            I(CODE),
            I(0),
        ],
    );
    let stock_w_name = t.text("Static", true);
    let stock_w = t.call(
        "CreateWindowExW",
        &[
            I(0),
            I(stock_w_name),
            I(0),
            I(WS_POPUP),
            I(0),
            I(0),
            I(10),
            I(10),
            I(0),
            I(0),
            I(CODE),
            I(0),
        ],
    );
    let old_a = t.call("GetWindowLongA", &[M(stock_a), I(GWL_WNDPROC as u32)]);
    let old_w = t.call(
        "SetWindowLongW",
        &[M(stock_w), I(GWL_WNDPROC as u32), I(wproc)],
    );
    let text = t.text("中", true);
    let stock_set = t.call(
        "CallWindowProcW",
        &[M(old_w), M(stock_w), I(wm::SETTEXT), I(0), I(text)],
    );
    t.run();
    assert_eq!((t.value(data), t.value(extra), t.value(de)), (0, 0, 0));
    assert_eq!(
        (t.value(read_a), t.value(read_extra)),
        (0x87654321, 0xfedcba98)
    );
    assert_eq!((t.value(zero), t.value(ze)), (0, 0x12345678));
    for (result, last, error) in failures {
        assert_eq!((t.value(result), t.value(last)), (0, error));
    }
    for (result, last) in [
        (cross_get, cge),
        (cross_a, cae),
        (cross_call, cce),
        (parent_a, pae),
    ] {
        assert_eq!((t.value(result), t.value(last)), (0, 50));
    }
    assert_eq!((t.value(final_w), t.value(final_a)), (wproc, aproc));
    let u = &t.machine.ext.get::<Win32>().user;
    assert!(
        u.windows
            .get(&t.machine.sys.objects, &t.value(w))
            .unwrap()
            .unicode
    );
    assert!(
        !u.windows
            .get(&t.machine.sys.objects, &t.value(a))
            .unwrap()
            .unicode
    );
    assert_eq!(t.value(old_a), t.value(old_w));
    assert_eq!(
        u.windows
            .get(&t.machine.sys.objects, &t.value(w))
            .unwrap()
            .owner,
        0
    );
    assert_eq!(
        u.windows
            .get(&t.machine.sys.objects, &t.value(a))
            .unwrap()
            .owner,
        0
    );
    assert_ne!(t.value(stock_set), 0);
    assert_eq!(
        u.windows
            .get(&t.machine.sys.objects, &t.value(stock_w))
            .unwrap()
            .text,
        "中"
    );
    let d = t.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(d.stub_calls, 0);
    assert!(
        d.warnings
            .keys()
            .any(|(api, _)| api == "user32.dll!SetWindowLongW")
    );
    assert!(
        d.warnings
            .keys()
            .any(|(api, _)| api == "user32.dll!CallWindowProcW")
    );
}

#[test]
fn window_long_w_fields_errors_and_cross_encoding_guards_preserve_user_owner() {
    errors(UnicornCpu::new(PARK_ADDR).unwrap());
    errors(JitCpu::new(PARK_ADDR, false).unwrap());
}
