//! Private client DCs retain one GDI owner until USER finishes window destruction.
use super::*;
use adv_device::surface::{Snapshot, SurfaceStore};
use adv_win32::gdi32::{
    raster::front::{DrawingProvider, set_provider},
    stock_object,
};

fn expect<B: TestCpu>(
    t: &mut Fixture<B>,
    expected: &mut Vec<(u32, u32)>,
    name: &str,
    args: &[Arg],
    value: u32,
) -> u32 {
    let pointer = t.call(name, args);
    expected.push((pointer, value));
    pointer
}

fn register<B: TestCpu>(t: &mut Fixture<B>, name: &str, style: u32, proc: u32) -> u32 {
    let name = t.text(name, false);
    let fields = [48, style, proc, 0, 0, CODE, 0, 0, 0, 0, name, 0];
    let info = t.bytes(
        &fields
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    t.call("RegisterClassExA", &[I(info)]);
    name
}

fn create<B: TestCpu>(t: &mut Fixture<B>, class: u32) -> u32 {
    t.call(
        "CreateWindowExA",
        &[
            I(0),
            I(class),
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

fn lifetime<B: TestCpu>(cpu: B, raster: bool) {
    let mut t = Fixture::new(cpu, 932);
    if raster {
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
    }
    let default = t.function("DefWindowProcA");
    let dying_dc = t.alloc(4);
    let dying_mode = t.alloc(4);
    let dying_font = t.alloc(4);
    let proc = t.procedure;
    let mut callback = Asm::new(proc);
    on_message(&mut callback, wm::NCDESTROY, |code| {
        invoke(code, t.function("GetDC"), &[S(4)]);
        code.mov_mem_eax(dying_dc);
        invoke(
            code,
            t.function("gdi32.dll!SetStretchBltMode"),
            &[M(dying_dc), I(3)],
        );
        code.mov_mem_eax(dying_mode);
        invoke(
            code,
            t.function("gdi32.dll!GetCurrentObject"),
            &[M(dying_dc), I(6)],
        );
        code.mov_mem_eax(dying_font);
        value(code, 0);
        code.ret_n(16);
    });
    callback.jmp(default);
    t.machine.cpu.write(proc, &callback.bytes).unwrap();
    t.machine.cpu.region(proc, callback.bytes.len());
    let own_class = register(&mut t, "PrivateClient", 0x20, proc);
    let common_class = register(&mut t, "CommonClient", 0, default);
    let first = create(&mut t, own_class);
    let second = create(&mut t, own_class);
    let common = create(&mut t, common_class);
    let ps = t.alloc(64);
    let point = t.alloc(8);
    let fixed = stock_object(11);
    let variable = stock_object(12);
    let system = stock_object(13);
    let mut expected = Vec::new();
    let mut same = Vec::new();
    let a = t.call("GetDC", &[M(first)]);
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(a), I(3)],
        1,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(a), I(0x112233)],
        0,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetBkColor",
        &[M(a), I(0x123456)],
        0xffffff,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetBkMode",
        &[M(a), I(1)],
        2,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SelectObject",
        &[M(a), I(fixed)],
        system,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetWindowOrgEx",
        &[M(a), I(2), I(3), I(0)],
        1,
    );
    expect(&mut t, &mut expected, "gdi32.dll!SaveDC", &[M(a)], 1);
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(a), I(2)],
        3,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(a), I(0x445566)],
        0x112233,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SelectObject",
        &[M(a), I(variable)],
        fixed,
    );
    expect(&mut t, &mut expected, "ReleaseDC", &[M(first), M(a)], 1);
    let again = t.call("GetDC", &[M(first)]);
    same.push((again, a));
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(again), I(2)],
        2,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(again), I(0x445566)],
        0x445566,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!RestoreDC",
        &[M(again), I(u32::MAX)],
        1,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(again), I(3)],
        3,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!GetCurrentObject",
        &[M(again), I(6)],
        fixed,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(again), I(0x112233)],
        0x112233,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetBkColor",
        &[M(again), I(0x123456)],
        0x123456,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetBkMode",
        &[M(again), I(1)],
        1,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!GetWindowOrgEx",
        &[M(again), I(point)],
        1,
    );
    let origin = t.save(point);
    let origin_y = t.save(point + 4);
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetWindowOrgEx",
        &[M(again), I(0), I(0), I(0)],
        1,
    );
    expect(&mut t, &mut expected, "ReleaseDC", &[M(first), M(again)], 1);
    expect(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(first), I(0), I(0)],
        1,
    );
    let paint = t.call("BeginPaint", &[M(first), I(ps)]);
    same.push((paint, a));
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(paint), I(3)],
        3,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!GetCurrentObject",
        &[M(paint), I(6)],
        fixed,
    );
    if raster {
        let rectangle = t.bytes(
            &[2i32, 0, 3, 1]
                .into_iter()
                .flat_map(i32::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        expect(
            &mut t,
            &mut expected,
            "FillRect",
            &[M(paint), I(rectangle), I(stock_object(0))],
            1,
        );
        let pixels = t.bytes(&[
            0x11, 0x22, 0x44, 0, 0xf0, 0x0f, 0x55, 0, 0xcc, 0xc3, 0x99, 0,
        ]);
        let info = t.bytes(
            &[40u32, 3, u32::MAX, 0x00200001, 0, 12, 0, 0, 0, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        expect(
            &mut t,
            &mut expected,
            "gdi32.dll!StretchDIBits",
            &[
                M(paint),
                I(0),
                I(0),
                I(2),
                I(1),
                I(0),
                I(0),
                I(3),
                I(1),
                I(pixels),
                I(info),
                I(0),
                I(0x00cc0020),
            ],
            1,
        );
    }
    expect(&mut t, &mut expected, "EndPaint", &[M(first), I(ps)], 1);
    expect(&mut t, &mut expected, "gdi32.dll!DeleteDC", &[M(a)], 0);
    let window_dc = t.call("GetWindowDC", &[M(first)]);
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(window_dc), I(2)],
        1,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(window_dc), I(0x777777)],
        0,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SelectObject",
        &[M(window_dc), I(variable)],
        system,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SaveDC",
        &[M(window_dc)],
        1,
    );
    expect(
        &mut t,
        &mut expected,
        "ReleaseDC",
        &[M(first), M(window_dc)],
        1,
    );
    let window_again = t.call("GetWindowDC", &[M(first)]);
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(window_again), I(2)],
        1,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(window_again), I(0)],
        0,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!GetCurrentObject",
        &[M(window_again), I(6)],
        system,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!RestoreDC",
        &[M(window_again), I(u32::MAX)],
        0,
    );
    expect(
        &mut t,
        &mut expected,
        "ReleaseDC",
        &[M(first), M(window_again)],
        1,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(a), I(3)],
        3,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!GetCurrentObject",
        &[M(a), I(6)],
        fixed,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(a), I(0x112233)],
        0x112233,
    );
    let b = t.call("GetDC", &[M(second)]);
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(b), I(2)],
        1,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(b), I(0xabcdef)],
        0,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SelectObject",
        &[M(b), I(variable)],
        system,
    );
    expect(&mut t, &mut expected, "ReleaseDC", &[M(second), M(b)], 1);
    let a_returned = t.call("GetDC", &[M(first)]);
    same.push((a_returned, a));
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(a_returned), I(3)],
        3,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(a_returned), I(0x112233)],
        0x112233,
    );
    expect(
        &mut t,
        &mut expected,
        "ReleaseDC",
        &[M(first), M(a_returned)],
        1,
    );
    let b_returned = t.call("GetDC", &[M(second)]);
    same.push((b_returned, b));
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(b_returned), I(2)],
        2,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(b_returned), I(0xabcdef)],
        0xabcdef,
    );
    expect(
        &mut t,
        &mut expected,
        "ReleaseDC",
        &[M(second), M(b_returned)],
        1,
    );
    let c = t.call("GetDC", &[M(common)]);
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(c), I(3)],
        1,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetBkMode",
        &[M(c), I(1)],
        2,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(c), I(0x778899)],
        0,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SelectObject",
        &[M(c), I(fixed)],
        system,
    );
    expect(&mut t, &mut expected, "gdi32.dll!SaveDC", &[M(c)], 1);
    expect(&mut t, &mut expected, "ReleaseDC", &[M(common), M(c)], 1);
    expect(
        &mut t,
        &mut expected,
        "InvalidateRect",
        &[M(common), I(0), I(0)],
        1,
    );
    let c_paint = t.call("BeginPaint", &[M(common), I(ps)]);
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(c_paint), I(3)],
        1,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetBkMode",
        &[M(c_paint), I(2)],
        2,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(c_paint), I(0)],
        0,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!GetCurrentObject",
        &[M(c_paint), I(6)],
        system,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!RestoreDC",
        &[M(c_paint), I(u32::MAX)],
        0,
    );
    expect(&mut t, &mut expected, "EndPaint", &[M(common), I(ps)], 1);
    expect(&mut t, &mut expected, "DestroyWindow", &[M(first)], 1);
    let destroyed_first = t.save(dying_dc);
    let first_mode = t.save(dying_mode);
    let first_font = t.save(dying_font);
    same.push((destroyed_first, a));
    expected.push((first_mode, 3));
    expected.push((first_font, fixed));
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!GetCurrentObject",
        &[M(a), I(6)],
        0,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(a), I(3)],
        0,
    );
    expect(&mut t, &mut expected, "GetDC", &[M(first)], 0);
    expect(
        &mut t,
        &mut expected,
        "kernel32.dll!GetLastError",
        &[],
        1400,
    );
    expect(&mut t, &mut expected, "ReleaseDC", &[M(first), M(a)], 0);
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetStretchBltMode",
        &[M(b), I(2)],
        2,
    );
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!SetTextColor",
        &[M(b), I(0xabcdef)],
        0xabcdef,
    );
    expect(&mut t, &mut expected, "DestroyWindow", &[M(second)], 1);
    same.push((dying_dc, b));
    expected.push((dying_mode, 2));
    expected.push((dying_font, variable));
    expect(
        &mut t,
        &mut expected,
        "gdi32.dll!GetCurrentObject",
        &[M(b), I(6)],
        0,
    );
    expect(&mut t, &mut expected, "DestroyWindow", &[M(common)], 1);
    t.run();
    assert_ne!(t.value(a), 0);
    assert_ne!(t.value(b), 0);
    assert_ne!(t.value(a), t.value(b));
    assert_ne!(t.value(a), t.value(window_dc));
    for (pointer, value) in expected {
        assert_eq!(t.value(pointer), value, "record {pointer:x}");
    }
    for (left, right) in same {
        assert_eq!(t.value(left), t.value(right));
    }
    assert_eq!((t.value(origin), t.value(origin_y)), (2, 3));
    let gdi = &t.machine.ext.get::<Win32>().gdi;
    assert!(gdi.get(&t.machine.sys.objects, t.value(a)).is_none());
    assert!(gdi.get(&t.machine.sys.objects, t.value(b)).is_none());
    if raster {
        let store = t.machine.ext.get::<SurfaceStore>();
        let surface = store.get(store.primary_id().unwrap()).unwrap();
        assert_eq!(
            t.read(surface.pixels, 12),
            [0x11, 0x22, 0x44, 0, 0xcc, 0xc3, 0x99, 0, 255, 255, 255, 0]
        );
        assert!(store.stats.presentations >= 2);
    }
    let diagnostics = t.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.warnings.is_empty());
}

#[test]
fn private_dc_lifetime_preserves_attributes_and_keeps_common_window_dcs_independent() {
    lifetime(UnicornCpu::new(PARK_ADDR).unwrap(), true);
    lifetime(JitCpu::new(PARK_ADDR, false).unwrap(), true);
}

#[test]
fn private_dc_cleanup_also_runs_without_a_graphics_provider() {
    fn cleanup<B: TestCpu>(cpu: B) {
        let mut t = Fixture::new(cpu, 932);
        let default = t.function("DefWindowProcA");
        let class = register(&mut t, "PrivateWithoutRaster", 0x20, default);
        let window = create(&mut t, class);
        let dc = t.call("GetDC", &[M(window)]);
        let mut expected = Vec::new();
        expect(
            &mut t,
            &mut expected,
            "gdi32.dll!SetTextColor",
            &[M(dc), I(0x123456)],
            0,
        );
        expect(&mut t, &mut expected, "ReleaseDC", &[M(window), M(dc)], 1);
        let again = t.call("GetDC", &[M(window)]);
        expect(
            &mut t,
            &mut expected,
            "gdi32.dll!SetTextColor",
            &[M(again), I(0x123456)],
            0x123456,
        );
        expect(&mut t, &mut expected, "DestroyWindow", &[M(window)], 1);
        expect(
            &mut t,
            &mut expected,
            "gdi32.dll!GetCurrentObject",
            &[M(dc), I(6)],
            0,
        );
        expect(&mut t, &mut expected, "ReleaseDC", &[M(window), M(dc)], 0);
        t.run();
        assert_ne!(t.value(dc), 0);
        assert_eq!(t.value(dc), t.value(again));
        for (pointer, value) in expected {
            assert_eq!(t.value(pointer), value);
        }
        assert!(
            t.machine
                .ext
                .get::<Win32>()
                .gdi
                .get(&t.machine.sys.objects, t.value(dc))
                .is_none()
        );
    }
    cleanup(UnicornCpu::new(PARK_ADDR).unwrap());
    cleanup(JitCpu::new(PARK_ADDR, false).unwrap());
}

#[test]
fn failed_first_surface_binding_does_not_publish_a_private_dc() {
    use super::paint_leases::direct;
    use adv_abi::Outcome;
    fn failure<B: TestCpu>(cpu: B) {
        let mut t = Fixture::new(cpu, 932);
        let arena = t
            .machine
            .sys
            .vm
            .alloc(&mut t.machine.cpu, 4096, Prot::RW)
            .unwrap();
        t.machine
            .ext
            .insert(SurfaceStore::new(100, 100, arena, 4096, Snapshot::default()).unwrap());
        set_provider(
            &mut t.machine.ext,
            DrawingProvider {
                memory: super::paint_leases::memory,
                completed: |_, _| None,
            },
        );
        let default = t.function("DefWindowProcA");
        let class = register(&mut t, "PrivateAllocationFailure", 0x20, default);
        let window = create(&mut t, class);
        t.run();
        let hwnd = t.value(window);
        let objects = t.machine.sys.objects.object_count();
        let dcs = t
            .machine
            .ext
            .get::<Win32>()
            .gdi
            .values(&t.machine.sys.objects)
            .count();
        for _ in 0..2 {
            assert!(matches!(direct(&mut t, "GetDC", &[hwnd]), Outcome::Fail(_)));
            assert_eq!(t.machine.sys.objects.object_count(), objects);
            assert_eq!(
                t.machine
                    .ext
                    .get::<Win32>()
                    .gdi
                    .values(&t.machine.sys.objects)
                    .count(),
                dcs
            );
        }
        let extra = t
            .machine
            .sys
            .vm
            .alloc(&mut t.machine.cpu, 65536, Prot::RW)
            .unwrap();
        t.machine
            .ext
            .get_mut::<SurfaceStore>()
            .extend_arena(extra, 65536)
            .unwrap();
        let Outcome::Return(dc) = direct(&mut t, "GetDC", &[hwnd]) else {
            panic!("retry with sufficient surface storage must succeed");
        };
        assert_ne!(dc, 0);
        assert!(matches!(
            direct(&mut t, "ReleaseDC", &[hwnd, dc]),
            Outcome::Return(1)
        ));
        assert!(matches!(direct(&mut t, "GetDC", &[hwnd]), Outcome::Return(value) if value == dc));
    }
    failure(UnicornCpu::new(PARK_ADDR).unwrap());
    failure(JitCpu::new(PARK_ADDR, false).unwrap());
}
