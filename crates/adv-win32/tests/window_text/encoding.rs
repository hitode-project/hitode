//! Real guest creation, procedure replacement and destruction observe USER's
//! current encoding, including callbacks while the HWND remains live.
use super::*;
use adv_win32::user32::window::{GWL_USERDATA, GWL_WNDPROC};

fn observing_proc<B: TestCpu>(
    test: &mut Fixture<B>,
    wide: bool,
    creation: u32,
    destruction: u32,
) -> u32 {
    let copied = test.alloc(64);
    let default = test.callback(wide, copied, None);
    let query = test.function("IsWindowUnicode");
    let start = test.procedure;
    let mut code = Asm::new(start);
    for (message, output) in [(wm::NCCREATE, creation), (wm::NCDESTROY, destruction)] {
        on_message(&mut code, message, |code| {
            invoke(code, query, &[S(4)]);
            code.mov_mem_eax(output);
        });
    }
    code.jmp(default);
    test.machine.cpu.write(start, &code.bytes).unwrap();
    test.machine.cpu.region(start, code.bytes.len());
    test.procedure = code.here().next_multiple_of(16);
    start
}

fn stock_window<B: TestCpu>(test: &mut Fixture<B>, wide: bool) -> u32 {
    let class = test.text("STATIC", wide);
    test.call(
        if wide {
            "CreateWindowExW"
        } else {
            "CreateWindowExA"
        },
        &[
            I(0),
            I(class),
            I(0),
            I(WS_POPUP),
            I(0),
            I(0),
            I(8),
            I(8),
            I(0),
            I(0),
            I(CODE),
            I(0),
        ],
    )
}

fn lifecycle<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu, 936);
    let registry = test.machine.sys.thunks.resolver();
    assert!(registry.implemented("user32.dll", "IsWindowUnicode"));
    assert_eq!(
        registry
            .host_fn("user32.dll", "IsWindowUnicode")
            .unwrap()
            .conv,
        Conv::Stdcall { argc: 1 }
    );
    let creation_a = test.bytes(&u32::MAX.to_le_bytes());
    let creation_w = test.bytes(&u32::MAX.to_le_bytes());
    let destruction_a = test.bytes(&u32::MAX.to_le_bytes());
    let destruction_w = test.bytes(&u32::MAX.to_le_bytes());
    let proc_a = observing_proc(&mut test, false, creation_a, destruction_a);
    let proc_w = observing_proc(&mut test, true, creation_w, destruction_w);
    let a = test.window(false, proc_a);
    // The existing fixture creates this through A using a registered W class:
    // the result must follow the class/procedure owner, not the creation API.
    let w = test.window(true, proc_w);
    let stock_a = stock_window(&mut test, false);
    let stock_w = stock_window(&mut test, true);
    test.set_error(0x1234);
    let before_a = test.call("IsWindowUnicode", &[M(a)]);
    let before_w = test.call("IsWindowUnicode", &[M(w)]);
    let builtin_a = test.call("IsWindowUnicode", &[M(stock_a)]);
    let builtin_w = test.call("IsWindowUnicode", &[M(stock_w)]);
    let preserved = test.error();
    let replacement_a = observing_proc(&mut test, false, creation_a, destruction_a);
    let replacement_w = observing_proc(&mut test, true, creation_w, destruction_w);
    let old_a = test.call(
        "SetWindowLongA",
        &[M(a), I(GWL_WNDPROC as u32), I(replacement_a)],
    );
    let old_w = test.call(
        "SetWindowLongW",
        &[M(w), I(GWL_WNDPROC as u32), I(replacement_w)],
    );
    // A W front on an unrelated field cannot turn an ANSI procedure into W.
    test.call(
        "SetWindowLongW",
        &[M(a), I(GWL_USERDATA as u32), I(0x12345678)],
    );
    test.set_error(0x1234);
    let after_a = test.call("IsWindowUnicode", &[M(a)]);
    let after_w = test.call("IsWindowUnicode", &[M(w)]);
    let subclass_error = test.error();
    let message = test.call("SendMessageW", &[M(w), I(0x8033), I(77), I(88)]);
    let destroy_w = test.call("DestroyWindow", &[M(w)]);
    let destroy_a = test.call("DestroyWindow", &[M(a)]);
    let mut invalid = Vec::new();
    for argument in [M(w), M(a), I(0), I(0xdeadbeef)] {
        test.set_error(0x1234);
        let result = test.call("IsWindowUnicode", &[argument]);
        let error = test.error();
        invalid.push((result, error));
    }
    test.call("DestroyWindow", &[M(stock_a)]);
    test.call("DestroyWindow", &[M(stock_w)]);
    test.run();
    assert_ne!(test.value(a), 0);
    assert_ne!(test.value(w), 0);
    assert_ne!(test.value(stock_a), 0);
    assert_ne!(test.value(stock_w), 0);
    assert_eq!((test.value(creation_a), test.value(creation_w)), (0, 1));
    assert_eq!((test.value(before_a), test.value(before_w)), (0, 1));
    assert_eq!((test.value(builtin_a), test.value(builtin_w)), (0, 1));
    assert_eq!(
        (test.value(preserved), test.value(subclass_error)),
        (0x1234, 0x1234)
    );
    assert_eq!((test.value(old_a), test.value(old_w)), (proc_a, proc_w));
    assert_eq!((test.value(after_a), test.value(after_w)), (0, 1));
    assert_eq!(test.value(message), 0xfedcba98);
    assert_eq!(
        test.rows(test.value(w), 0x8033),
        [[test.value(w), 0x8033, 77, 88]]
    );
    assert_eq!((test.value(destroy_a), test.value(destroy_w)), (1, 1));
    // Windows remain valid through WM_NCDESTROY; the owner retires afterward.
    assert_eq!(
        (test.value(destruction_a), test.value(destruction_w)),
        (0, 1)
    );
    for (result, error) in invalid {
        assert_eq!((test.value(result), test.value(error)), (0, 1400));
    }
    let diagnostics = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.warnings.is_empty());
}

#[test]
fn window_unicode_query_observes_live_guest_classes_stock_controls_and_subclass_lifecycle() {
    lifecycle(UnicornCpu::new(PARK_ADDR).unwrap());
    lifecycle(JitCpu::new(PARK_ADDR, false).unwrap());
    lifecycle(JitCpu::new(PARK_ADDR, true).unwrap());
}
