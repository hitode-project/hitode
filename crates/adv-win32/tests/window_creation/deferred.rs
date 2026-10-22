//! Deferred edits publish their geometry together after guest changing callbacks.
use super::*;
fn deferred<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let capture = f.alloc(368);
    let base = f.callback(true, capture, None);
    let handles = f.alloc(8);
    let observed = f.alloc(64);
    let saved = f.alloc(64);
    let mode = f.alloc(4);
    let start = f.procedure;
    let mut proc = Asm::new(start);
    let get = f.function("GetWindowRect");
    for (message, offset) in [(wm::WINDOWPOSCHANGING, 0), (wm::WINDOWPOSCHANGED, 32)] {
        on_message(&mut proc, message, |code| {
            invoke(code, get, &[M(handles), I(observed + offset)]);
            invoke(code, get, &[M(handles + 4), I(observed + offset + 16)]);
            if message == wm::WINDOWPOSCHANGING {
                code.mov_eax_mem(mode);
                code.bytes.extend_from_slice(&[0x85, 0xc0]);
                skip(code, 0x84, |code| {
                    // Guest edits ignored fields while adding NOMOVE/NOSIZE.
                    code.mov_eax_esp(16);
                    code.bytes.extend_from_slice(&[0xc7, 0x40, 8]);
                    code.bytes.extend_from_slice(&1234u32.to_le_bytes());
                    code.bytes.extend_from_slice(&[0xc7, 0x40, 16]);
                    code.bytes.extend_from_slice(&999u32.to_le_bytes());
                    code.bytes.extend_from_slice(&[0x83, 0x48, 24, 3]);
                });
            }
        });
    }
    proc.jmp(base);
    f.machine.cpu.write(start, &proc.bytes).unwrap();
    f.machine.cpu.region(start, proc.bytes.len());
    f.procedure = proc.here().next_multiple_of(16);
    let class = f.class(start, "deferred geometry", true);
    let a = geometry::create(&mut f, class, I(0), 0, [10, 10, 100, 100], false);
    f.code.mov_eax_mem(a).mov_mem_eax(handles);
    let b = geometry::create(&mut f, class, I(0), 0, [20, 20, 100, 100], false);
    f.code.mov_eax_mem(b).mov_mem_eax(handles + 4);
    let batch = f.call("BeginDeferWindowPos", &[I(0)]);
    let first = f.call(
        "DeferWindowPos",
        &[M(batch), M(a), I(0), I(20), I(30), I(40), I(50), I(0x14)],
    );
    let second = f.call(
        "DeferWindowPos",
        &[M(first), M(b), I(0), I(60), I(70), I(80), I(90), I(0x14)],
    );
    let third = f.call(
        "DeferWindowPos",
        &[M(second), M(a), I(0), I(25), I(35), I(999), I(999), I(0x15)],
    );
    let before = f.alloc(16);
    f.call("GetWindowRect", &[M(a), I(before)]);
    let log_start = f.alloc(4);
    f.code.mov_eax_mem(DATA).mov_mem_eax(log_start);
    let ended = f.call("EndDeferWindowPos", &[M(third)]);
    let log_end = f.alloc(4);
    f.code.mov_eax_mem(DATA).mov_mem_eax(log_end);
    for i in 0..16 {
        f.code
            .mov_eax_mem(observed + i * 4)
            .mov_mem_eax(saved + i * 4);
    }
    let repeated = f.call("EndDeferWindowPos", &[M(third)]);
    let stale_error = f.call("kernel32.dll!GetLastError", &[]);
    f.code.mov_mem_imm(mode, 1);
    let no_move = f.call(
        "SetWindowPos",
        &[M(a), I(0), I(100), I(100), I(200), I(200), I(0x14)],
    );
    f.code.mov_mem_imm(mode, 0);
    let after = f.alloc(16);
    f.call("GetWindowRect", &[M(a), I(after)]);
    let failed = f.call("BeginDeferWindowPos", &[I(1)]);
    let valid = f.call(
        "DeferWindowPos",
        &[
            M(failed),
            M(a),
            I(0),
            I(200),
            I(200),
            I(300),
            I(300),
            I(0x14),
        ],
    );
    let invalid = f.call(
        "DeferWindowPos",
        &[
            M(valid),
            I(0xdeadbeef),
            I(0),
            I(0),
            I(0),
            I(0),
            I(0),
            I(0x14),
        ],
    );
    let invalid_error = f.call("kernel32.dll!GetLastError", &[]);
    let invalid_end = f.call("EndDeferWindowPos", &[M(valid)]);
    let untouched = f.alloc(16);
    f.call("GetWindowRect", &[M(a), I(untouched)]);
    let negative = f.call("BeginDeferWindowPos", &[I(u32::MAX)]);
    let negative_error = f.call("kernel32.dll!GetLastError", &[]);
    let huge = f.call("BeginDeferWindowPos", &[I(i32::MAX as u32)]);
    let huge_error = f.call("kernel32.dll!GetLastError", &[]);
    let empty = f.call("BeginDeferWindowPos", &[I(0)]);
    let empty_end = f.call("EndDeferWindowPos", &[M(empty)]);
    let child = geometry::create(&mut f, class, M(a), 0, [1, 1, 10, 10], true);
    let mixed = f.call("BeginDeferWindowPos", &[I(2)]);
    let with_child = f.call(
        "DeferWindowPos",
        &[M(mixed), M(child), I(0), I(2), I(3), I(4), I(5), I(0x14)],
    );
    let mixed_result = f.call(
        "DeferWindowPos",
        &[M(with_child), M(a), I(0), I(2), I(3), I(4), I(5), I(0x14)],
    );
    let mixed_error = f.call("kernel32.dll!GetLastError", &[]);
    let mixed_end = f.call("EndDeferWindowPos", &[M(mixed)]);
    let overflow = f.call("BeginDeferWindowPos", &[I(1)]);
    let overflow_result = f.call(
        "DeferWindowPos",
        &[
            M(overflow),
            M(a),
            I(0),
            I(i32::MAX as u32),
            I(0),
            I(1),
            I(1),
            I(0x14),
        ],
    );
    let overflow_error = f.call("kernel32.dll!GetLastError", &[]);
    let ignored = f.call(
        "SetWindowPos",
        &[
            M(a),
            I(0),
            I(i32::MAX as u32),
            I(i32::MAX as u32),
            I(i32::MAX as u32),
            I(i32::MAX as u32),
            I(0x17),
        ],
    );
    f.run();
    for (slot, expected) in [
        (mixed_result, 0),
        (mixed_error, 87),
        (mixed_end, 0),
        (overflow_result, 0),
        (overflow_error, 87),
        (ignored, 1),
    ] {
        assert_eq!(f.value(slot), expected);
    }
    for slot in [batch, first, second, third] {
        assert_ne!(f.value(slot), 0);
    }
    for (slot, value) in [
        (ended, 1),
        (repeated, 0),
        (stale_error, 1405),
        (no_move, 1),
        (invalid, 0),
        (invalid_error, 1400),
        (invalid_end, 0),
        (negative, 0),
        (negative_error, 87),
        (huge, 0),
        (huge_error, 8),
        (empty_end, 1),
    ] {
        assert_eq!(f.value(slot), value, "result {slot:x}");
    }
    let rect = |at| (0..4).map(|i| f.value(at + i * 4)).collect::<Vec<_>>();
    assert_eq!(rect(before), [10, 10, 110, 110]);
    assert_eq!(rect(saved), [10, 10, 110, 110]);
    assert_eq!(rect(saved + 16), [20, 20, 120, 120]);
    assert_eq!(rect(saved + 32), [25, 35, 65, 85]);
    assert_eq!(rect(saved + 48), [60, 70, 140, 160]);
    assert_eq!(rect(after), [25, 35, 65, 85]);
    assert_eq!(rect(untouched), [25, 35, 65, 85]);
    let notifications: Vec<_> = (f.value(log_start)..f.value(log_end))
        .step_by(16)
        .filter(|&at| [wm::WINDOWPOSCHANGING, wm::WINDOWPOSCHANGED].contains(&f.value(at + 4)))
        .map(|at| (f.value(at), f.value(at + 4)))
        .collect();
    assert_eq!(
        notifications,
        [
            (f.value(a), wm::WINDOWPOSCHANGING),
            (f.value(b), wm::WINDOWPOSCHANGING),
            (f.value(a), wm::WINDOWPOSCHANGED),
            (f.value(b), wm::WINDOWPOSCHANGED)
        ]
    );
    assert!(
        f.machine
            .ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .is_empty()
    );
}
#[test]
// spec: WIN32-USER-6 WIN32-USER-7
fn batches_are_atomic_honor_edited_flags_and_retire_failed_handles() {
    deferred(JitCpu::new(PARK_ADDR, false).unwrap());
    deferred(JitCpu::new(PARK_ADDR, true).unwrap());
    deferred(UnicornCpu::new(PARK_ADDR).unwrap());
}
