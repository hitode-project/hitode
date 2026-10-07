//! Native isolated-desktop evidence: user-update-native/native-isolated.jsonl.
//! Authored guest RECTs and erase callbacks exercise the real USER/DC owner.
use super::*;
use adv_device::surface::{Snapshot, SurfaceStore};
use adv_win32::gdi32::raster::front::{DrawingProvider, set_provider};

fn rect<B: TestCpu>(t: &mut Fixture<B>, r: [i32; 4]) -> u32 {
    t.bytes(&r.into_iter().flat_map(i32::to_le_bytes).collect::<Vec<_>>())
}
fn quad<B: TestCpu>(t: &Fixture<B>, p: u32) -> [i32; 4] {
    std::array::from_fn(|i| t.value(p + i as u32 * 4) as i32)
}
fn check_call<B: TestCpu>(
    t: &mut Fixture<B>,
    expected: &mut Vec<(u32, u32)>,
    name: &str,
    args: &[Arg],
    value: u32,
) {
    let result = t.call(name, args);
    expected.push((result, value));
}
fn region<B: TestCpu>(cpu: B) {
    let mut t = Fixture::new(cpu, 932);
    let arena = t
        .machine
        .sys
        .vm
        .alloc(&mut t.machine.cpu, 131072, Prot::RW)
        .unwrap();
    t.machine
        .ext
        .insert(SurfaceStore::new(100, 100, arena, 131072, Snapshot::default()).unwrap());
    set_provider(
        &mut t.machine.ext,
        DrawingProvider {
            memory: super::paint_leases::memory,
            completed: |_, _| None,
        },
    );
    let full = rect(&mut t, [0, 0, 100, 100]);
    let empty = rect(&mut t, [2, 2, 2, 7]);
    let outside = rect(&mut t, [110, 110, 120, 120]);
    let reversed = rect(&mut t, [9, 8, 2, 3]);
    let a = rect(&mut t, [20, 10, 25, 15]);
    let b = rect(&mut t, [40, 20, 45, 25]);
    let hole = rect(&mut t, [21, 11, 24, 14]);
    let erasing = rect(&mut t, [60, 30, 65, 35]);
    let newer = rect(&mut t, [70, 35, 75, 40]);
    let spare = t.alloc(16);
    let reenter = t.alloc(4);
    let erase_count = t.alloc(4);
    let erase_return = t.alloc(4);
    t.code.mov_mem_imm(erase_return, 1);
    let paint_count = t.alloc(4);
    let callback_pending = t.alloc(4);
    let callback_fill = t.alloc(4);
    let callback_rect = t.alloc(16);
    let color = t.call("gdi32.dll!CreateSolidBrush", &[I(0xff)]);
    let mut callback = Asm::new(t.procedure);
    on_message(&mut callback, wm::ERASEBKGND, |code| {
        code.mov_eax_mem(erase_count).bytes.push(0x40); // inc eax
        code.mov_mem_eax(erase_count);
        invoke(
            code,
            t.function("GetUpdateRect"),
            &[S(4), I(callback_rect), I(0)],
        );
        code.mov_mem_eax(callback_pending);
        invoke(
            code,
            t.function("FillRect"),
            &[S(12), I(full), I(adv_win32::gdi32::stock_object(4))],
        );
        code.mov_mem_eax(callback_fill);
        code.cmp_mem_imm8(reenter, 1);
        conditional(
            code,
            |code| {
                code.mov_mem_imm(reenter, 0);
                invoke(code, t.function("InvalidateRect"), &[S(4), I(newer), I(1)]);
            },
            false,
        );
        code.mov_eax_mem(erase_return);
        code.ret_n(16);
    });
    on_message(&mut callback, wm::PAINT, |code| {
        code.mov_eax_mem(paint_count).bytes.push(0x40);
        code.mov_mem_eax(paint_count);
    });
    callback.jmp(t.function("DefWindowProcA"));
    let proc = t.procedure;
    t.machine.cpu.write(proc, &callback.bytes).unwrap();
    t.machine.cpu.region(proc, callback.bytes.len());
    t.procedure = callback.here().next_multiple_of(16);
    let window = t.window(false, proc);
    t.call("ShowWindow", &[M(window), I(1)]);
    let mut expected = Vec::new();
    let initial_ps = t.alloc(64);
    let initial_dc = t.call("BeginPaint", &[M(window), I(initial_ps)]);
    expected.push((initial_dc, 1));
    check_call(
        &mut t,
        &mut expected,
        "EndPaint",
        &[M(window), I(initial_ps)],
        1,
    );
    let after_initial_erase = t.save(erase_count);
    check_call(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(window), I(empty), I(1)],
        1,
    );
    check_call(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(window), I(outside), I(1)],
        1,
    );
    let none = t.alloc(16);
    check_call(
        &mut t,
        &mut expected,
        "GetUpdateRect",
        &[M(window), I(none), I(0)],
        0,
    );
    check_call(&mut t, &mut expected, "UpdateWindow", &[M(window)], 1);
    let after_empty_paints = t.save(paint_count);
    let message = t.alloc(28);
    check_call(
        &mut t,
        &mut expected,
        "PeekMessageA",
        &[I(message), M(window), I(wm::PAINT), I(wm::PAINT), I(1)],
        0,
    );
    check_call(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(window), I(reversed), I(0)],
        1,
    );
    let partial_query = t.alloc(16);
    check_call(
        &mut t,
        &mut expected,
        "GetUpdateRect",
        &[M(window), I(partial_query), I(0)],
        1,
    );
    let ps_partial = t.alloc(64);
    let before_paint_dc = t.call("GetDC", &[M(window)]);
    check_call(
        &mut t,
        &mut expected,
        "gdi32.dll!SaveDC",
        &[M(before_paint_dc)],
        1,
    );
    let partial = t.call("BeginPaint", &[M(window), I(ps_partial)]);
    check_call(
        &mut t,
        &mut expected,
        "gdi32.dll!RestoreDC",
        &[M(before_paint_dc), I(1)],
        1,
    );
    check_call(
        &mut t,
        &mut expected,
        "FillRect",
        &[M(partial), I(full), M(color)],
        1,
    );
    check_call(
        &mut t,
        &mut expected,
        "GetUpdateRect",
        &[M(window), I(0), I(0)],
        0,
    );
    check_call(
        &mut t,
        &mut expected,
        "EndPaint",
        &[M(window), I(ps_partial)],
        1,
    );
    check_call(
        &mut t,
        &mut expected,
        "ReleaseDC",
        &[M(window), M(before_paint_dc)],
        1,
    );
    check_call(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(window), I(a), I(0)],
        1,
    );
    check_call(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(window), I(b), I(0)],
        1,
    );
    check_call(
        &mut t,
        &mut expected,
        "ValidateRect",
        &[M(window), I(hole)],
        1,
    );
    let ps_union = t.alloc(64);
    let union = t.call("BeginPaint", &[M(window), I(ps_union)]);
    check_call(&mut t, &mut expected, "gdi32.dll!SaveDC", &[M(union)], 1);
    check_call(
        &mut t,
        &mut expected,
        "gdi32.dll!SetWindowOrgEx",
        &[M(union), I(7), I(9), I(0)],
        1,
    );
    check_call(
        &mut t,
        &mut expected,
        "gdi32.dll!RestoreDC",
        &[M(union), I(1)],
        1,
    );
    check_call(
        &mut t,
        &mut expected,
        "FillRect",
        &[M(union), I(full), M(color)],
        1,
    );
    check_call(
        &mut t,
        &mut expected,
        "gdi32.dll!RectVisible",
        &[M(union), I(hole)],
        0,
    );
    check_call(
        &mut t,
        &mut expected,
        "EndPaint",
        &[M(window), I(ps_union)],
        1,
    );
    let erased_before_reentry = t.save(erase_count);
    check_call(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(window), I(erasing), I(1)],
        1,
    );
    t.code.mov_mem_imm(reenter, 1);
    let ps_reenter = t.alloc(64);
    t.call("BeginPaint", &[M(window), I(ps_reenter)]);
    let inside_bp_pending = t.save(callback_pending);
    check_call(
        &mut t,
        &mut expected,
        "EndPaint",
        &[M(window), I(ps_reenter)],
        1,
    );
    let new_query = t.alloc(16);
    check_call(
        &mut t,
        &mut expected,
        "GetUpdateRect",
        &[M(window), I(new_query), I(0)],
        1,
    );
    // Failed guards preserve the complete existing update region and erase bit.
    check_call(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(window), I(READONLY + 4090), I(1)],
        0,
    );
    check_call(&mut t, &mut expected, "kernel32.dll!GetLastError", &[], 87);
    check_call(
        &mut t,
        &mut expected,
        "ValidateRect",
        &[M(window), I(READONLY + 4090)],
        0,
    );
    check_call(
        &mut t,
        &mut expected,
        "GetUpdateRect",
        &[M(window), I(READONLY), I(1)],
        0,
    );
    let after_guards_erases = t.save(erase_count);
    check_call(
        &mut t,
        &mut expected,
        "GetUpdateRect",
        &[M(window), I(spare), I(1)],
        1,
    );
    let inside_get_pending = t.save(callback_pending);
    let after_get_erases = t.save(erase_count);
    let ps_after_query = t.alloc(64);
    t.call("BeginPaint", &[M(window), I(ps_after_query)]);
    check_call(
        &mut t,
        &mut expected,
        "EndPaint",
        &[M(window), I(ps_after_query)],
        1,
    );
    let final_erases = t.save(erase_count);
    // Native failure-to-erase remains observable through fErase on a later
    // FALSE invalidation, without sending a second unsolicited erase message.
    t.code.mov_mem_imm(erase_return, 0);
    check_call(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(window), I(erasing), I(1)],
        1,
    );
    let failed_erase_ps = t.alloc(64);
    t.call("BeginPaint", &[M(window), I(failed_erase_ps)]);
    check_call(
        &mut t,
        &mut expected,
        "EndPaint",
        &[M(window), I(failed_erase_ps)],
        1,
    );
    check_call(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(window), I(erasing), I(0)],
        1,
    );
    let still_unerased_ps = t.alloc(64);
    t.call("BeginPaint", &[M(window), I(still_unerased_ps)]);
    check_call(
        &mut t,
        &mut expected,
        "EndPaint",
        &[M(window), I(still_unerased_ps)],
        1,
    );
    let no_repeated_erase = t.save(erase_count);
    t.code.mov_mem_imm(erase_return, 1);
    check_call(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(window), I(erasing), I(1)],
        1,
    );
    let recovered_ps = t.alloc(64);
    t.call("BeginPaint", &[M(window), I(recovered_ps)]);
    check_call(
        &mut t,
        &mut expected,
        "EndPaint",
        &[M(window), I(recovered_ps)],
        1,
    );
    // 65 isolated rectangles must fail honestly without discarding the old 64.
    for i in 0..64 {
        let x = (i % 8) * 10;
        let y = (i / 8) * 10;
        let r = rect(&mut t, [x, y, x + 1, y + 1]);
        check_call(
            &mut t,
            &mut expected,
            "InvalidateRect",
            &[M(window), I(r), I(0)],
            1,
        );
    }
    let excess = rect(&mut t, [90, 90, 91, 91]);
    check_call(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(window), I(excess), I(1)],
        0,
    );
    check_call(&mut t, &mut expected, "kernel32.dll!GetLastError", &[], 50);
    let capacity_bbox = t.alloc(16);
    check_call(
        &mut t,
        &mut expected,
        "GetUpdateRect",
        &[M(window), I(capacity_bbox), I(0)],
        1,
    );
    check_call(&mut t, &mut expected, "ValidateRect", &[M(window), I(0)], 1);
    check_call(
        &mut t,
        &mut expected,
        "GetUpdateRect",
        &[M(window), I(0), I(0)],
        0,
    );
    check_call(
        &mut t,
        &mut expected,
        "gdi32.dll!DeleteObject",
        &[M(color)],
        1,
    );
    t.run();
    for (p, v) in expected {
        assert_eq!(t.value(p), v, "result {p:x}");
    }
    assert_eq!(t.value(after_initial_erase), 1);
    assert_eq!(t.value(after_empty_paints), 0);
    assert_eq!(quad(&t, none), [0; 4]);
    assert_eq!(quad(&t, partial_query), [2, 3, 9, 8]);
    assert_eq!(quad(&t, ps_partial + 8), [2, 3, 9, 8]);
    assert_eq!(t.value(ps_partial + 4), 0);
    assert_eq!(quad(&t, ps_union + 8), [20, 10, 45, 25]);
    assert_eq!(t.value(erased_before_reentry), 1);
    assert_eq!(t.value(inside_bp_pending), 0);
    assert_eq!(quad(&t, ps_reenter + 8), [60, 30, 65, 35]);
    assert_eq!(t.value(ps_reenter + 4), 0);
    assert_eq!(quad(&t, new_query), [70, 35, 75, 40]);
    assert_eq!(t.value(after_guards_erases), 2);
    assert_eq!(t.value(inside_get_pending), 1);
    assert_eq!(t.value(after_get_erases), 3);
    assert_eq!(t.value(final_erases), 3);
    assert_eq!(t.value(failed_erase_ps + 4), 1);
    assert_eq!(t.value(still_unerased_ps + 4), 1);
    assert_eq!(t.value(no_repeated_erase), 4);
    assert_eq!(t.value(recovered_ps + 4), 0);
    assert_eq!(t.value(callback_fill), 1);
    assert_eq!(quad(&t, ps_after_query + 8), [70, 35, 75, 40]);
    assert_eq!(t.value(ps_after_query + 4), 0);
    assert_eq!(quad(&t, capacity_bbox), [0, 0, 71, 71]);
    let store = t.machine.ext.get::<SurfaceStore>();
    let surface = store.get(store.primary_id().unwrap()).unwrap();
    let (pixels, pitch) = (surface.pixels, surface.pitch);
    for y in 0..100 {
        for x in 0..100 {
            let red = (2..9).contains(&x) && (3..8).contains(&y)
                || ((20..25).contains(&x)
                    && (10..15).contains(&y)
                    && !((21..24).contains(&x) && (11..14).contains(&y)))
                || (40..45).contains(&x) && (20..25).contains(&y);
            assert_eq!(
                t.read(pixels + y * pitch + x * 4, 4),
                if red { [0, 0, 255, 0] } else { [0, 0, 0, 0] },
                "pixel {x},{y}"
            );
        }
    }
    assert!(
        t.machine
            .ext
            .get::<Win32>()
            .gdi
            .get(&t.machine.sys.objects, 1)
            .is_none()
    );
    assert_eq!(t.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
}

#[test]
fn exact_update_rectangles_erase_reentry_and_paint_clip_share_the_guest_window_owner() {
    region(UnicornCpu::new(PARK_ADDR).unwrap());
    region(JitCpu::new(PARK_ADDR, false).unwrap());
    region(JitCpu::new(PARK_ADDR, true).unwrap());
}
