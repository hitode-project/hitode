//! Public W queue fronts use the same real guest procedures and MSG layout.
use super::*;

fn authored_message<B: TestCpu>(
    test: &mut Fixture<B>,
    window: u32,
    message: u32,
    wp: u32,
    lp: u32,
) -> u32 {
    let pointer = test.bytes(
        &[0, message, wp, lp, 123, 7, 9]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    test.code.mov_eax_mem(window).mov_mem_eax(pointer);
    pointer
}

fn retrieval<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu, 1252);
    let copied = test.alloc(64);
    let proc_w = test.callback(true, copied, None);
    let proc_a = test.callback(false, copied, None);
    let w = test.window(true, proc_w);
    let a = test.window(false, proc_a);
    let out = test.alloc(28);
    test.set_error(0x1234);
    test.call(
        "PostMessageA",
        &[M(w), I(0x8033), I(0x87654321), I(0xfedc1234)],
    );
    let peek = test.call("PeekMessageW", &[I(out), M(w), I(0x8033), I(0x8033), I(0)]);
    let peek_wp = test.save(out + 8);
    let get = test.call("GetMessageW", &[I(out), M(w), I(0x8033), I(0x8033)]);
    let opaque = test.call("DispatchMessageW", &[I(out)]);
    let unchanged_error = test.error();

    // PostMessageA supplies an ANSI character. Retrieval produces UTF-16,
    // while dispatch honors the receiving class's A/W procedure encoding.
    test.call("PostMessageA", &[M(w), I(wm::CHAR), I(0x80), I(1)]);
    test.call("GetMessageW", &[I(out), M(w), I(wm::CHAR), I(wm::CHAR)]);
    let unicode = test.save(out + 8);
    test.call("DispatchMessageW", &[I(out)]);
    test.call("PostMessageA", &[M(a), I(wm::CHAR), I(0x80), I(2)]);
    test.call("GetMessageW", &[I(out), M(a), I(wm::CHAR), I(wm::CHAR)]);
    test.call("DispatchMessageW", &[I(out)]);
    test.call("PostMessageA", &[M(w), I(wm::CHAR), I(0x80), I(3)]);
    test.call("GetMessageA", &[I(out), M(w), I(wm::CHAR), I(wm::CHAR)]);
    let ansi = test.save(out + 8);
    test.call("DispatchMessageA", &[I(out)]);

    // WM_QUIT bypasses the window/message filters and is retained by a peek.
    test.call("PostQuitMessage", &[I(0xfffffffe)]);
    let quit_peek = test.call("PeekMessageW", &[I(out), M(w), I(1), I(1), I(0)]);
    let quit_message = test.save(out + 4);
    let quit_code = test.save(out + 8);
    let quit = test.call("GetMessageW", &[I(out), M(a), I(1), I(1)]);
    let quit_dispatch = test.call("DispatchMessageW", &[I(out)]);
    let empty = test.call("PeekMessageW", &[I(out), I(0), I(0x7ffe), I(0x7ffe), I(1)]);
    let after_empty = test.save(out + 8);
    test.run();

    assert_eq!(
        (test.value(peek), test.value(get), test.value(peek_wp)),
        (1, 1, 0x87654321)
    );
    assert_eq!(test.value(opaque), 0xfedcba98);
    assert_eq!(test.value(unchanged_error), 0x1234);
    assert_eq!(
        test.rows(test.value(w), 0x8033),
        [[test.value(w), 0x8033, 0x87654321, 0xfedc1234]]
    );
    assert_eq!((test.value(unicode), test.value(ansi)), (0x20ac, 0x80));
    assert_eq!(
        test.rows(test.value(w), wm::CHAR),
        [
            [test.value(w), wm::CHAR, 0x20ac, 1],
            [test.value(w), wm::CHAR, 0x20ac, 3]
        ]
    );
    assert_eq!(
        test.rows(test.value(a), wm::CHAR),
        [[test.value(a), wm::CHAR, 0x80, 2]]
    );
    assert_eq!(
        (
            test.value(quit_peek),
            test.value(quit_message),
            test.value(quit_code)
        ),
        (1, wm::QUIT, 0xfffffffe)
    );
    assert_eq!(
        (
            test.value(quit),
            test.value(quit_dispatch),
            test.value(empty),
            test.value(after_empty)
        ),
        (0, 0, 0, 0xfffffffe)
    );
    assert_eq!(
        test.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls,
        0
    );
}

#[test]
fn unicode_queue_fronts_preserve_peek_quit_opaque_results_and_class_character_encoding() {
    retrieval(UnicornCpu::new(PARK_ADDR).unwrap());
    retrieval(JitCpu::new(PARK_ADDR, false).unwrap());
    retrieval(JitCpu::new(PARK_ADDR, true).unwrap());
}

fn guarded_dispatch<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu, 936);
    let copied_a = test.alloc(64);
    let copied_w = test.alloc(64);
    let reenter = test.bytes(&1_u32.to_le_bytes());
    let nested = test.alloc(4);
    let proc_a = test.callback(false, copied_a, Some((reenter, nested)));
    let proc_w = test.callback(true, copied_w, None);
    let a = test.window(false, proc_a);
    let w = test.window(true, proc_w);
    let text = test.text("中A", true);
    let text_msg = authored_message(&mut test, a, wm::SETTEXT, 0, text);
    let set = test.call("DispatchMessageW", &[I(text_msg)]);
    let wide_char = authored_message(&mut test, w, wm::CHAR, 0x4e2d, 0);
    test.call("DispatchMessageW", &[I(wide_char)]);
    let surrogate = authored_message(&mut test, w, wm::CHAR, 0xd83d, 0);
    test.call("DispatchMessageW", &[I(surrogate)]);
    let unsupported = authored_message(&mut test, a, wm::CHAR, 0x4e2d, 0);
    let refused = test.call("DispatchMessageW", &[I(unsupported)]);
    let refused_error = test.error();

    let out = test.alloc(28);
    test.call("PostMessageA", &[M(w), I(0x8033), I(7), I(9)]);
    let readonly_peek = test.call(
        "PeekMessageW",
        &[I(READONLY), M(w), I(0x8033), I(0x8033), I(1)],
    );
    let peek_error = test.error();
    let readonly_get = test.call("GetMessageW", &[I(READONLY), M(w), I(0x8033), I(0x8033)]);
    let get_error = test.error();
    let invalid = test.call("GetMessageW", &[I(out), I(0xdeadbeef), I(0), I(0)]);
    let invalid_error = test.error();
    let retrieved = test.call("GetMessageW", &[I(out), M(w), I(0x8033), I(0x8033)]);
    let pending = test.call("DispatchMessageW", &[I(out)]);
    let bad_dispatch = test.call("DispatchMessageW", &[I(0xfffffff0)]);
    let dispatch_error = test.error();
    // A lone CP936 lead byte cannot be converted without a second character
    // message. The refused W retrieval leaves it available to its A caller.
    test.call("PostMessageA", &[M(a), I(wm::CHAR), I(0xd6), I(0)]);
    let split = test.call("GetMessageW", &[I(out), M(a), I(wm::CHAR), I(wm::CHAR)]);
    let split_error = test.error();
    let kept = test.call("GetMessageA", &[I(out), M(a), I(wm::CHAR), I(wm::CHAR)]);
    let kept_byte = test.save(out + 8);
    test.run();

    assert_eq!(test.value(set), 0x55);
    assert_eq!(test.value(nested), 3); // Reentry observes the old title.
    let mut encoded = test.machine.ext.get::<Win32>().codec.encode("中A");
    encoded.push(0);
    assert_eq!(test.read(copied_a, encoded.len()), encoded);
    for row in test.rows(test.value(a), wm::SETTEXT) {
        assert_ne!(row[3], text);
        assert_eq!(test.machine.ext.get::<adv_os::Heaps>().owner(row[3]), None);
    }
    assert_eq!(
        test.rows(test.value(w), wm::CHAR),
        [
            [test.value(w), wm::CHAR, 0x4e2d, 0],
            [test.value(w), wm::CHAR, 0xd83d, 0]
        ]
    );
    assert!(test.rows(test.value(a), wm::CHAR).is_empty());
    assert_eq!((test.value(refused), test.value(refused_error)), (0, 50));
    assert_eq!(
        (test.value(readonly_peek), test.value(peek_error)),
        (0, 998)
    );
    assert_eq!(
        (test.value(readonly_get), test.value(get_error)),
        (u32::MAX, 998)
    );
    assert_eq!(
        (test.value(invalid), test.value(invalid_error)),
        (u32::MAX, 1400)
    );
    assert_eq!(
        (test.value(retrieved), test.value(pending)),
        (1, 0xfedcba98)
    );
    assert_eq!(
        (test.value(bad_dispatch), test.value(dispatch_error)),
        (0, 998)
    );
    assert_eq!((test.value(split), test.value(split_error)), (u32::MAX, 50));
    assert_eq!((test.value(kept), test.value(kept_byte)), (1, 0xd6));
    assert_eq!(test.read(READONLY, 28), vec![0; 28]);
    let diagnostics = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(
        diagnostics
            .warnings
            .keys()
            .any(|(api, reason)| api == "user32.dll!DispatchMessageW"
                && reason.contains("multibyte"))
    );
}

#[test]
fn unicode_dispatch_converts_text_with_reentry_and_checks_msg_buffers_without_consuming() {
    guarded_dispatch(UnicornCpu::new(PARK_ADDR).unwrap());
    guarded_dispatch(JitCpu::new(PARK_ADDR, false).unwrap());
    guarded_dispatch(JitCpu::new(PARK_ADDR, true).unwrap());
}
