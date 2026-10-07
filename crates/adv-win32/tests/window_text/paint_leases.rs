//! Reentrant guest WM_PAINT retains the actual shared DC, not just its handle.
use super::*;
use adv_abi::{Env, Outcome};
use adv_cpu::backend::Guest;
use adv_device::surface::{MemoryOperations, MemoryView, Snapshot, SurfaceStore};
use adv_win32::gdi32::raster::front::{DrawingProvider, set_provider};
use std::borrow::Cow;

pub(super) fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, address, length| {
                let mut bytes = vec![0; length];
                cpu.read(address, &mut bytes).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(bytes))
            },
            write: |cpu, address, bytes| cpu.write(address, bytes).map_err(|e| e.to_string()),
            writable: |cpu, address, length| {
                adv_abi::guest::check_range(cpu, address, length as u32, Prot::WRITE)
                    .map_err(|e| e.to_string())
            },
            copy: |cpu, source, dest, length| {
                cpu.copy_memory(source, dest, length)
                    .map_err(|e| e.to_string())
            },
        },
    )
}
fn record<B: TestCpu>(t: &mut Fixture<B>, code: &mut Asm, name: &str, args: &[Arg]) -> u32 {
    let result = t.alloc(4);
    invoke(code, t.function(name), args);
    code.mov_mem_eax(result);
    result
}
fn direct<B: TestCpu>(t: &mut Fixture<B>, name: &str, args: &[u32]) -> Outcome {
    let (module, name) = name.split_once('!').unwrap_or(("user32.dll", name));
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let api = registry.get(module, name).unwrap();
    let old = t.machine.cpu.reg(adv_cpu::backend::Reg::Esp);
    let stack = DATA + 0x7000;
    for (index, &arg) in args.iter().enumerate() {
        t.machine
            .cpu
            .write_u32(stack + 4 + index as u32 * 4, arg)
            .unwrap();
    }
    t.machine.cpu.set_reg(adv_cpu::backend::Reg::Esp, stack);
    let result = (api.handler)(&mut Env {
        context: t.machine.sys.current,
        cpu: &mut t.machine.cpu,
        sys: &mut t.machine.sys,
        ext: &mut t.machine.ext,
        thunk: 0,
    });
    t.machine.cpu.set_reg(adv_cpu::backend::Reg::Esp, old);
    result
}
fn nested<B: TestCpu>(cpu: B) {
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
            memory,
            completed: |_, _| None,
        },
    );
    let outer_ps = t.alloc(64);
    let inner_ps = t.alloc(64);
    let unmatched_ps = t.alloc(64);
    let denied_ps = t.bytes(&[0xa5; 64]);
    let point = t.alloc(8);
    let current = t.alloc(8);
    let depth = t.alloc(4);
    let outer_esp = t.alloc(4);
    let resumed_esp = t.alloc(4);
    let hole = t.bytes(
        &[0i32, 0, 1, 1]
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let static_class = t.text("Static", false);
    let other = t.call(
        "CreateWindowExA",
        &[
            I(0),
            I(static_class),
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
    );
    let black = adv_win32::gdi32::stock_object(4);
    let white = adv_win32::gdi32::stock_object(0);
    let mut expected = Vec::new();
    let procedure = t.procedure;
    let mut code = Asm::new(procedure);
    on_message(&mut code, wm::PAINT, |code| {
        code.cmp_mem_imm8(depth, 0);
        conditional(
            code,
            |code| {
                let inner = record(&mut t, code, "BeginPaint", &[S(4), I(inner_ps)]);
                expected.push((inner, 1));
                expected.push((record(&mut t, code, "gdi32.dll!SaveDC", &[M(inner)]), 2));
                expected.push((
                    record(
                        &mut t,
                        code,
                        "gdi32.dll!SelectObject",
                        &[M(inner), I(white)],
                    ),
                    black,
                ));
                expected.push((
                    record(
                        &mut t,
                        code,
                        "gdi32.dll!SetWindowOrgEx",
                        &[M(inner), I(9), I(9), I(0)],
                    ),
                    1,
                ));
                expected.push((
                    record(&mut t, code, "gdi32.dll!RestoreDC", &[M(inner), I(2)]),
                    1,
                ));
                expected.push((record(&mut t, code, "EndPaint", &[S(4), I(inner_ps)]), 1));
                value(code, 0);
                code.ret_n(16);
            },
            true,
        );
        code.mov_mem_imm(depth, 1);
        let dc = record(&mut t, code, "BeginPaint", &[S(4), I(outer_ps)]);
        expected.push((dc, 1));
        expected.push((record(&mut t, code, "gdi32.dll!SaveDC", &[M(dc)]), 1));
        expected.push((
            record(&mut t, code, "gdi32.dll!SelectObject", &[M(dc), I(black)]),
            white,
        ));
        expected.push((
            record(
                &mut t,
                code,
                "gdi32.dll!SetWindowOrgEx",
                &[M(dc), I((-4i32) as u32), I((-5i32) as u32), I(0)],
            ),
            1,
        ));
        expected.push((
            record(
                &mut t,
                code,
                "gdi32.dll!ExcludeClipRect",
                &[M(dc), I(0), I(0), I(2), I(2)],
            ),
            3,
        ));
        expected.push((record(&mut t, code, "GetDC", &[S(4)]), 1));
        expected.push((record(&mut t, code, "GetWindowDC", &[S(4)]), 1));
        expected.push((record(&mut t, code, "ReleaseDC", &[M(other), M(dc)]), 0));
        expected.push((record(&mut t, code, "gdi32.dll!DeleteDC", &[M(dc)]), 0));
        expected.push((record(&mut t, code, "GetDC", &[M(other)]), 0));
        expected.push((record(&mut t, code, "kernel32.dll!GetLastError", &[]), 50));
        expected.push((
            record(&mut t, code, "BeginPaint", &[M(other), I(denied_ps)]),
            0,
        ));
        expected.push((record(&mut t, code, "kernel32.dll!GetLastError", &[]), 50));
        code.mov_mem_imm(unmatched_ps, 1);
        expected.push((
            record(&mut t, code, "EndPaint", &[S(4), I(unmatched_ps)]),
            1,
        ));
        code.bytes.extend_from_slice(&[0x89, 0x25]);
        code.bytes.extend_from_slice(&outer_esp.to_le_bytes());
        expected.push((
            record(&mut t, code, "InvalidateRect", &[S(4), I(0), I(0)]),
            1,
        ));
        expected.push((record(&mut t, code, "UpdateWindow", &[S(4)]), 1));
        code.bytes.extend_from_slice(&[0x89, 0x25]);
        code.bytes.extend_from_slice(&resumed_esp.to_le_bytes());
        // A duplicate inner EndPaint cannot consume the still-live outer paint.
        expected.push((record(&mut t, code, "EndPaint", &[S(4), I(inner_ps)]), 1));
        expected.push((
            record(&mut t, code, "gdi32.dll!GetCurrentObject", &[M(dc), I(2)]),
            black,
        ));
        expected.push((
            record(&mut t, code, "gdi32.dll!GetWindowOrgEx", &[M(dc), I(point)]),
            1,
        ));
        expected.push((
            record(&mut t, code, "gdi32.dll!RectVisible", &[M(dc), I(hole)]),
            0,
        ));
        expected.push((
            record(
                &mut t,
                code,
                "gdi32.dll!SetPixel",
                &[M(dc), I(0), I(0), I(0xff)],
            ),
            u32::MAX,
        ));
        expected.push((
            record(
                &mut t,
                code,
                "gdi32.dll!SetPixel",
                &[M(dc), I(2), I(0), I(0xff)],
            ),
            0xff,
        ));
        expected.push((
            record(
                &mut t,
                code,
                "gdi32.dll!GetCurrentPositionEx",
                &[M(dc), I(current)],
            ),
            1,
        ));
        expected.push((
            record(&mut t, code, "gdi32.dll!RestoreDC", &[M(dc), I(1)]),
            1,
        ));
        expected.push((
            record(&mut t, code, "gdi32.dll!SelectObject", &[M(dc), I(black)]),
            white,
        ));
        expected.push((
            record(
                &mut t,
                code,
                "gdi32.dll!SetPixel",
                &[M(dc), I(0), I(0), I(0xff0000)],
            ),
            0xff0000,
        ));
        expected.push((record(&mut t, code, "EndPaint", &[S(4), I(outer_ps)]), 1));
        expected.push((
            record(&mut t, code, "gdi32.dll!GetCurrentObject", &[M(dc), I(2)]),
            black,
        ));
        expected.push((record(&mut t, code, "ReleaseDC", &[S(4), M(dc)]), 1));
        expected.push((
            record(&mut t, code, "gdi32.dll!GetCurrentObject", &[M(dc), I(2)]),
            black,
        ));
        expected.push((record(&mut t, code, "ReleaseDC", &[S(4), M(dc)]), 1));
        expected.push((
            record(&mut t, code, "gdi32.dll!GetCurrentObject", &[M(dc), I(2)]),
            0,
        ));
        expected.push((record(&mut t, code, "ReleaseDC", &[S(4), M(dc)]), 0));
        value(code, 0);
        code.ret_n(16);
    });
    code.jmp(t.function("DefWindowProcA"));
    t.machine.cpu.write(procedure, &code.bytes).unwrap();
    t.machine.cpu.region(procedure, code.bytes.len());
    t.procedure = code.here().next_multiple_of(16);
    let hwnd = t.window(false, procedure);
    t.call("ShowWindow", &[M(hwnd), I(1)]);
    let update = t.call("InvalidateRect", &[M(hwnd), I(0), I(0)]);
    let painted = t.call("UpdateWindow", &[M(hwnd)]);
    t.run();
    assert_eq!((t.value(update), t.value(painted)), (1, 1));
    for (pointer, value) in expected {
        assert_eq!(t.value(pointer), value, "result {pointer:x}");
    }
    assert_eq!(t.value(outer_esp), t.value(resumed_esp));
    assert_eq!(
        t.read(point, 8),
        [(-4i32).to_le_bytes(), (-5i32).to_le_bytes()].concat()
    );
    assert_eq!(t.read(current, 8), [0; 8]);
    assert_eq!(t.read(denied_ps, 64), [0xa5; 64]);
    let gdi = &t.machine.ext.get::<Win32>().gdi;
    assert!(gdi.get(&t.machine.sys.objects, 1).is_none());
    let surface = t.machine.ext.get::<SurfaceStore>().primary_id().unwrap();
    let surface = t.machine.ext.get::<SurfaceStore>().get(surface).unwrap();
    let pixels = surface.pixels;
    assert_ne!(t.read(pixels + (5 * 100 + 6) * 4, 4), [0; 4]);
    assert_eq!(t.read(pixels + (5 * 100 + 4) * 4, 4), [0; 4]);
    assert_ne!(t.read(pixels, 4), [0; 4]);
    // Checked publication failure after a valid GetDC cannot retire that lease.
    let hwnd = t.value(hwnd);
    assert!(matches!(
        direct(&mut t, "GetDC", &[hwnd]),
        Outcome::Return(1)
    ));
    assert!(matches!(
        direct(&mut t, "BeginPaint", &[hwnd, READONLY]),
        Outcome::Fail(_)
    ));
    assert!(
        t.machine
            .ext
            .get::<Win32>()
            .gdi
            .get(&t.machine.sys.objects, 1)
            .is_some()
    );
    assert!(matches!(
        direct(&mut t, "ReleaseDC", &[hwnd, 1]),
        Outcome::Return(1)
    ));
    assert!(
        t.machine
            .ext
            .get::<Win32>()
            .gdi
            .get(&t.machine.sys.objects, 1)
            .is_none()
    );
    assert_eq!(t.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
    assert!(
        t.machine
            .ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, _)| api == "user32.dll!BeginPaint")
    );
}

#[test]
fn nested_guest_paint_and_get_dc_leases_preserve_saved_state_until_the_last_release() {
    nested(UnicornCpu::new(PARK_ADDR).unwrap());
    nested(JitCpu::new(PARK_ADDR, false).unwrap());
    nested(JitCpu::new(PARK_ADDR, true).unwrap());
}
