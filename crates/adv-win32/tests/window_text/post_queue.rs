//! Authored asynchronous posts retain their source ABI in the common queue.
use super::*;

fn posting<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu, 936);
    let out = test.alloc(28);
    let tid = test.call("kernel32.dll!GetCurrentThreadId", &[]);
    test.set_error(0x12345678);
    let self_post = test.call("PostThreadMessageW", &[M(tid), I(0x8033), I(7), I(9)]);
    let self_error = test.error();
    test.call("GetMessageW", &[I(out), I(u32::MAX), I(0x8033), I(0x8033)]);
    let self_hwnd = test.save(out);
    let self_wp = test.save(out + 8);
    let self_dispatch = test.call("DispatchMessageW", &[I(out)]);
    let copied = test.alloc(64);
    let proc_w = test.callback(true, copied, None);
    let proc_a = test.callback(false, copied, None);
    let w = test.window(true, proc_w);
    let a = test.window(false, proc_a);

    // Step40's actual opaque WM_MOUSEMOVE parameters use the same posted queue.
    test.set_error(0x12345678);
    let mouse = test.call("PostMessageW", &[M(w), I(0x200), I(0), I(0x01700380)]);
    let mouse_error = test.error();
    let peek = test.call("PeekMessageA", &[I(out), M(w), I(0x200), I(0x200), I(0)]);
    let mouse_lp = test.save(out + 12);
    test.call("GetMessageW", &[I(out), M(w), I(0x200), I(0x200)]);
    test.call("DispatchMessageW", &[I(out)]);

    // Source W does not depend on the destination class. A retrieval and then
    // A dispatch round-trip Euro through the supported single-byte ACP profile.
    test.call("PostMessageW", &[M(w), I(wm::CHAR), I(0x20ac), I(1)]);
    test.call("GetMessageA", &[I(out), M(w), I(wm::CHAR), I(wm::CHAR)]);
    let ansi = test.save(out + 8);
    test.call("DispatchMessageA", &[I(out)]);
    test.call("PostMessageW", &[M(a), I(wm::CHAR), I(0x20ac), I(2)]);
    test.call("GetMessageW", &[I(out), M(a), I(wm::CHAR), I(wm::CHAR)]);
    let unicode = test.save(out + 8);
    test.call("DispatchMessageW", &[I(out)]);
    test.call("PostMessageA", &[M(w), I(wm::CHAR), I(0x80), I(3)]);
    test.call("GetMessageW", &[I(out), M(w), I(wm::CHAR), I(wm::CHAR)]);
    test.call("DispatchMessageW", &[I(out)]);
    // Split DBCS retrieval remains explicitly unsupported. A failed A query
    // does not consume the W post; its original UTF-16 unit is still retrievable.
    test.call("PostMessageW", &[M(w), I(wm::CHAR), I(0x3042), I(4)]);
    let split = test.call("GetMessageA", &[I(out), M(w), I(wm::CHAR), I(wm::CHAR)]);
    let split_error = test.error();
    let kept = test.call("GetMessageW", &[I(out), M(w), I(wm::CHAR), I(wm::CHAR)]);
    let kept_unit = test.save(out + 8);
    test.call("DispatchMessageW", &[I(out)]);

    let broadcast = test.call("PostMessageW", &[I(0xffff), I(0x8033), I(42), I(24)]);
    for window in [w, a] {
        test.call("GetMessageW", &[I(out), M(window), I(0x8033), I(0x8033)]);
        test.call("DispatchMessageW", &[I(out)]);
    }
    let mut failures = Vec::new();
    for (name, args, expected) in [
        (
            "PostMessageW",
            vec![I(0xdeadbeef), I(0x200), I(0), I(0)],
            1400,
        ),
        (
            "PostMessageA",
            vec![I(0xdeadbeef), I(0x200), I(0), I(0)],
            1400,
        ),
        (
            "PostThreadMessageW",
            vec![I(0), I(0x8033), I(0), I(0)],
            1444,
        ),
        (
            "PostThreadMessageA",
            vec![I(u32::MAX), I(0x8033), I(0), I(0)],
            1444,
        ),
        ("PostMessageW", vec![M(w), I(wm::SETTEXT), I(0), I(0)], 1159),
        ("PostMessageA", vec![M(a), I(wm::GETTEXT), I(0), I(0)], 1159),
        (
            "PostThreadMessageW",
            vec![M(tid), I(0x4a), I(0), I(0)],
            1159,
        ),
        (
            "PostThreadMessageA",
            vec![M(tid), I(wm::NCCREATE), I(0), I(0)],
            1159,
        ),
    ] {
        let result = test.call(name, &args);
        let error = test.error();
        failures.push((result, error, expected));
    }

    // A live kernel-only worker has no USER queue. Only its own Peek/Get creates
    // one; successful W posts then wake the genuine blocked GetMessageW.
    let reached = test.call("kernel32.dll!CreateEventW", &[I(0), I(1), I(0), I(0)]);
    let gate = test.call("kernel32.dll!CreateEventW", &[I(0), I(1), I(0), I(0)]);
    let ready = test.call("kernel32.dll!CreateEventW", &[I(0), I(1), I(0), I(0)]);
    let worker_tid = test.alloc(4);
    let worker_msg = test.alloc(28);
    let worker_address = CODE + 0xf000;
    let mut worker = Asm::new(worker_address);
    for (name, args) in [
        ("kernel32.dll!SetEvent", vec![M(reached)]),
        (
            "kernel32.dll!WaitForSingleObject",
            vec![M(gate), I(u32::MAX)],
        ),
        ("PeekMessageW", vec![I(worker_msg), I(0), I(0), I(0), I(0)]),
        ("kernel32.dll!SetEvent", vec![M(ready)]),
        (
            "GetMessageW",
            vec![I(worker_msg), I(0), I(wm::CHAR), I(wm::CHAR)],
        ),
    ] {
        invoke(&mut worker, test.function(name), &args);
    }
    value(&mut worker, 7);
    worker.ret_n(4);
    test.machine
        .cpu
        .write(worker_address, &worker.bytes)
        .unwrap();
    test.machine.cpu.region(worker_address, worker.bytes.len());
    let thread = test.call(
        "kernel32.dll!CreateThread",
        &[I(0), I(0), I(worker_address), I(0), I(0), I(worker_tid)],
    );
    test.call(
        "kernel32.dll!WaitForSingleObject",
        &[M(reached), I(u32::MAX)],
    );
    let no_queue = test.call(
        "PostThreadMessageW",
        &[M(worker_tid), I(wm::CHAR), I(0x20ac), I(8)],
    );
    let no_queue_error = test.error();
    test.call("kernel32.dll!SetEvent", &[M(gate)]);
    test.call("kernel32.dll!WaitForSingleObject", &[M(ready), I(u32::MAX)]);
    test.set_error(0x12345678);
    let live = test.call(
        "PostThreadMessageW",
        &[M(worker_tid), I(wm::CHAR), I(0x20ac), I(8)],
    );
    let live_error = test.error();
    test.call(
        "kernel32.dll!WaitForSingleObject",
        &[M(thread), I(u32::MAX)],
    );
    let dead = test.call(
        "PostThreadMessageW",
        &[M(worker_tid), I(0x8033), I(0), I(0)],
    );
    let dead_error = test.error();
    for handle in [thread, reached, gate, ready] {
        test.call("kernel32.dll!CloseHandle", &[M(handle)]);
    }
    test.run();

    assert_eq!(
        (
            test.value(self_post),
            test.value(self_error),
            test.value(self_hwnd),
            test.value(self_wp),
            test.value(self_dispatch)
        ),
        (1, 0x12345678, 0, 7, 0)
    );
    assert_eq!(
        (
            test.value(mouse),
            test.value(mouse_error),
            test.value(peek),
            test.value(mouse_lp)
        ),
        (1, 0x12345678, 1, 0x01700380)
    );
    assert_eq!(
        test.rows(test.value(w), 0x200),
        [[test.value(w), 0x200, 0, 0x01700380]]
    );
    assert_eq!((test.value(ansi), test.value(unicode)), (0x80, 0x20ac));
    assert_eq!(
        test.rows(test.value(w), wm::CHAR),
        [
            [test.value(w), wm::CHAR, 0x20ac, 1],
            [test.value(w), wm::CHAR, 0x20ac, 3],
            [test.value(w), wm::CHAR, 0x3042, 4]
        ]
    );
    assert_eq!(
        test.rows(test.value(a), wm::CHAR),
        [[test.value(a), wm::CHAR, 0x80, 2]]
    );
    assert_eq!(
        (
            test.value(split),
            test.value(split_error),
            test.value(kept),
            test.value(kept_unit)
        ),
        (u32::MAX, 50, 1, 0x3042)
    );
    assert_eq!(test.value(broadcast), 1);
    for window in [w, a] {
        assert_eq!(
            test.rows(test.value(window), 0x8033),
            [[test.value(window), 0x8033, 42, 24]]
        );
    }
    for (result, error, expected) in failures {
        assert_eq!((test.value(result), test.value(error)), (0, expected));
    }
    assert_eq!(
        (test.value(no_queue), test.value(no_queue_error)),
        (0, 1444)
    );
    assert_eq!((test.value(live), test.value(live_error)), (1, 0x12345678));
    assert_eq!((test.value(dead), test.value(dead_error)), (0, 1444));
    assert_eq!(
        [
            test.value(worker_msg),
            test.value(worker_msg + 4),
            test.value(worker_msg + 8),
            test.value(worker_msg + 12)
        ],
        [0, wm::CHAR, 0x20ac, 8]
    );
    let diagnostics = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    for name in [
        "PostMessageA",
        "PostMessageW",
        "PostThreadMessageA",
        "PostThreadMessageW",
    ] {
        assert!(
            diagnostics
                .warnings
                .keys()
                .any(|key| key.0 == format!("user32.dll!{name}")
                    && key.1.contains("synchronous delivery"))
        );
    }
}

fn quota<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu, 1252);
    // Populate the same owner directly as authored pre-existing traffic; the
    // API still makes both the failure and the one-slot recovery guest calls.
    let queue = test
        .machine
        .ext
        .get_mut::<Win32>()
        .user
        .queues
        .entry(0)
        .or_default();
    for index in 0..10_000 {
        let mut message = adv_win32::user32::message::Msg::default();
        message.msg = 0x8033;
        message.wparam = index;
        queue.post(message);
    }
    let out = test.alloc(28);
    let full = test.call("PostMessageW", &[I(0), I(0x8033), I(10_000), I(42)]);
    let full_error = test.error();
    test.call(
        "PeekMessageW",
        &[I(out), I(u32::MAX), I(0x8033), I(0x8033), I(1)],
    );
    let first = test.save(out + 8);
    test.set_error(0x12345678);
    let recovered = test.call("PostMessageW", &[I(0), I(0x8033), I(10_000), I(42)]);
    let recovered_error = test.error();
    test.run();
    assert_eq!((test.value(full), test.value(full_error)), (0, 1816));
    assert_eq!(test.value(first), 0);
    assert_eq!(
        (test.value(recovered), test.value(recovered_error)),
        (1, 0x12345678)
    );
    let queue = &test.machine.ext.get::<Win32>().user.queues[&0];
    assert_eq!(queue.posted.len(), 10_000);
    assert_eq!(queue.posted.front().unwrap().wparam, 1);
    assert_eq!(
        (
            queue.posted.back().unwrap().wparam,
            queue.posted.back().unwrap().lparam
        ),
        (10_000, 42)
    );
}

#[test]
fn posts_share_queue_wakeup_character_encoding_native_failures_and_bounded_capacity() {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for api in [
        "PostMessageA",
        "PostMessageW",
        "PostThreadMessageA",
        "PostThreadMessageW",
    ] {
        assert_eq!(
            registry.get("user32.dll", api).unwrap().conv,
            Conv::Stdcall { argc: 4 }
        );
        assert!(registry.dynamic_available("user32.dll", api));
    }
    assert!(registry.signature_conflicts().is_empty());
    posting(UnicornCpu::new(PARK_ADDR).unwrap());
    posting(JitCpu::new(PARK_ADDR, false).unwrap());
    posting(JitCpu::new(PARK_ADDR, true).unwrap());
    quota(UnicornCpu::new(PARK_ADDR).unwrap());
    quota(JitCpu::new(PARK_ADDR, false).unwrap());
    quota(JitCpu::new(PARK_ADDR, true).unwrap());
}
