//! Native masks and typed argument transport over the same DC/surface owner.
use super::*;
use adv_cpu::backend::Reg;

mod clipbox;
mod display_dc;
mod drawdib;
mod set_dib;

fn call(m: &mut Machine<JitCpu>, data: u32, api: &str, args: &[u32]) -> u32 {
    let (module, name) = api.split_once('!').unwrap();
    let mut reg = Registry::default();
    crate::register(&mut reg);
    let host = reg.get(module, name).unwrap();
    assert_eq!(
        host.conv,
        adv_abi::Conv::Stdcall {
            argc: args.len() as u8
        }
    );
    assert!(reg.dynamic_available(module, name));
    m.cpu.write(data + 512, &[0; 64]).unwrap();
    for (i, &arg) in args.iter().enumerate() {
        m.cpu.write_u32(data + 516 + i as u32 * 4, arg).unwrap();
    }
    let esp = m.cpu.reg(Reg::Esp);
    m.cpu.set_reg(Reg::Esp, data + 512);
    let result = (host.handler)(&mut env(m));
    m.cpu.set_reg(Reg::Esp, esp);
    match result {
        Outcome::Return(v) => v,
        Outcome::Fail(e) => panic!("{api}: {e}"),
        _ => panic!("unexpected continuation"),
    }
}
fn rect(m: &mut Machine<JitCpu>, data: u32, r: [i32; 4]) {
    m.cpu
        .write(
            data,
            &r.into_iter().flat_map(i32::to_le_bytes).collect::<Vec<_>>(),
        )
        .unwrap();
}
fn masks(m: &Machine<JitCpu>, pixels: u32) -> [u8; 8] {
    let mut raw = [0; 256];
    m.cpu.read(pixels, &mut raw).unwrap();
    std::array::from_fn(|y| {
        (0..8).fold(0, |a, x| {
            a | if raw[(y * 8 + x) * 4..(y * 8 + x + 1) * 4]
                .iter()
                .any(|&b| b != 0)
            {
                1 << x
            } else {
                0
            }
        })
    })
}

#[test]
fn frame_exclusion_save_restore_and_transfer_share_the_actual_pixels() {
    let (mut m, data, dc, pixels) = fixture(8, 8, PixelFormat::Bgra32, false);
    let before = m.sys.objects.object_count();
    crate::util::set_last_error(&mut env(&mut m), 0x12345678);
    assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!ExcludeClipRect", &[dc, 5, 5, 2, 2]),
        3
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
    rect(&mut m, data, [0, 0, 8, 8]);
    assert_eq!(
        call(
            &mut m,
            data,
            "user32.dll!FillRect",
            &[dc, data, stock_object(0)]
        ),
        1
    );
    assert_eq!(masks(&m, pixels), [255, 255, 227, 227, 227, 255, 255, 255]);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetPixel", &[dc, 3, 3, 0xff]),
        u32::MAX
    );
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetPixel", &[dc, 3, 3]),
        u32::MAX
    );
    m.cpu.write(pixels, &[0; 256]).unwrap();
    for (r, expected, ret) in [
        ([1, 1, 6, 6], [0, 62, 34, 34, 34, 62, 0, 0], 1),
        ([2, 2, 3, 6], [0, 0, 0, 0, 0, 4, 0, 0], 1), // middle is excluded
        ([5, 5, 2, 2], [0; 8], 0),
    ] {
        m.cpu.write(pixels, &[0; 256]).unwrap();
        rect(&mut m, data, r);
        assert_eq!(
            call(
                &mut m,
                data,
                "user32.dll!FrameRect",
                &[dc, data, stock_object(0)]
            ),
            ret
        );
        assert_eq!(masks(&m, pixels), expected);
    }
    assert_eq!(
        call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
        1
    );
    // Degenerate border strips are a native USER rule, not an empty-rectangle fill.
    for (r, expected) in [
        ([2, 2, 2, 5], [0, 0, 0, 6, 0, 0, 0, 0]),
        ([2, 2, 5, 2], [0, 28, 28, 0, 0, 0, 0, 0]),
        ([2, 2, 2, 2], [0, 6, 6, 0, 0, 0, 0, 0]),
    ] {
        m.cpu.write(pixels, &[0; 256]).unwrap();
        rect(&mut m, data, r);
        assert_eq!(
            call(
                &mut m,
                data,
                "user32.dll!FrameRect",
                &[dc, data, stock_object(0)]
            ),
            1
        );
        assert_eq!(masks(&m, pixels), expected);
    }
    // Overlapping self-BitBlt snapshots its source before splitting the row.
    for x in 0..8 {
        m.cpu.write_u32(pixels + x * 4, x + 1).unwrap();
    }
    assert_eq!(
        call(&mut m, data, "gdi32.dll!ExcludeClipRect", &[dc, 3, 0, 5, 1]),
        3
    );
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!BitBlt",
            &[dc, 1, 0, 7, 1, dc, 0, 0, 0xcc0020]
        ),
        1
    );
    let row = (0..8)
        .map(|x| m.cpu.read_u32(pixels + x * 4).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(row, [1, 1, 2, 4, 5, 5, 6, 7]);
    // A separate source row verifies clipped stretching retains global sampling.
    let mut contexts = adv_os::gdi::DeviceContexts::new(m.ext.get::<Win32>().gdi.clone());
    let (src, src_dc, src_pixels) = {
        let store = m.ext.get_mut::<SurfaceStore>();
        let src = store
            .create(8, 8, false, PixelFormat::Bgra32, &mut memory(&mut m.cpu))
            .unwrap();
        let src_dc = store.new_id();
        (src, src_dc, store.get(src).unwrap().pixels)
    };
    assert!(contexts.create(&mut m.sys.objects, src_dc, src));
    for x in 0..8 {
        m.cpu.write_u32(src_pixels + x * 4, 0x10 + x).unwrap();
    }
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetStretchBltMode", &[dc, 3]),
        1
    );
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!StretchBlt",
            &[dc, 2, 0, 4, 1, src_dc, 0, 0, 8, 1, 0xcc0020]
        ),
        1
    );
    assert_eq!(m.cpu.read_u32(pixels + 8).unwrap(), 0x10);
    assert_eq!(m.cpu.read_u32(pixels + 12).unwrap(), 4);
    assert_eq!(m.cpu.read_u32(pixels + 16).unwrap(), 5);
    assert_eq!(m.cpu.read_u32(pixels + 20).unwrap(), 0x16);
    assert_eq!(call(&mut m, data, "gdi32.dll!DeleteDC", &[src_dc]), 1);
    assert_eq!(call(&mut m, data, "gdi32.dll!DeleteObject", &[src]), 1);
    assert_eq!(m.sys.objects.object_count(), before);
}

#[test]
fn frame_guards_clip_capacity_and_provider_fail_without_partial_pixels_or_state() {
    let (mut m, data, dc, pixels) = fixture(256, 5, PixelFormat::Bgra32, false);
    m.cpu.write(pixels, &[0xa5; 5120]).unwrap();
    let short = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
    m.cpu.protect(short + 4096, 4096, Prot(0)).unwrap();
    for pointer in [0, 0xfffffff8, short + 4096 - 8] {
        assert_eq!(
            call(
                &mut m,
                data,
                "user32.dll!FrameRect",
                &[dc, pointer, stock_object(0)]
            ),
            0
        );
    }
    rect(&mut m, data, [0, 0, 256, 5]);
    m.cpu.protect(pixels + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(
        call(
            &mut m,
            data,
            "user32.dll!FrameRect",
            &[dc, data, stock_object(0)]
        ),
        0
    );
    let mut raw = [0; 5120];
    m.cpu.read(pixels, &mut raw).unwrap();
    assert_eq!(raw, [0xa5; 5120]);
    assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
    // Completely clipped readonly pixels are never validated as a future write.
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!ExcludeClipRect",
            &[dc, 0, 0, 256, 5]
        ),
        1
    );
    assert_eq!(
        call(
            &mut m,
            data,
            "user32.dll!FrameRect",
            &[dc, data, stock_object(0)]
        ),
        1
    );
    assert_eq!(
        call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
        1
    );
    for x in 0..64 {
        assert_eq!(
            call(
                &mut m,
                data,
                "gdi32.dll!ExcludeClipRect",
                &[dc, 1000 + x, 0, 1001 + x, 1]
            ),
            3
        );
    }
    let before = super::super::super::clip::snapshot(&env(&mut m), dc);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!ExcludeClipRect", &[dc, 0, 0, 1, 1]),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    assert!(before.contains(0, 0));
    assert!(super::super::super::clip::snapshot(&env(&mut m), dc).contains(0, 0));
    super::super::super::front::install(&mut m);
    assert_eq!(
        call(
            &mut m,
            data,
            "user32.dll!FrameRect",
            &[dc, data, stock_object(0)]
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!ExcludeClipRect", &[dc, 0, 0, 1, 1]),
        0
    );
    super::super::super::front::set_provider(
        &mut m.ext,
        super::super::super::front::DrawingProvider {
            memory,
            completed: |_, _| None,
        },
    );
    // No second cache/owner survives deleting the DC.
    assert_eq!(call(&mut m, data, "gdi32.dll!DeleteDC", &[dc]), 1);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!ExcludeClipRect", &[dc, 0, 0, 1, 1]),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 6);
}

#[test]
fn rect_visible_matches_native_normalization_degenerate_axes_and_saved_clip_without_writes() {
    let (mut m, data, dc, pixels) = fixture(8, 8, PixelFormat::Bgra32, false);
    let count = m.sys.objects.object_count();
    m.cpu.write(pixels, &[0xa5; 256]).unwrap();
    assert_eq!(
        call(&mut m, data, "gdi32.dll!ExcludeClipRect", &[dc, 2, 2, 5, 5]),
        3
    );
    for (r, expected) in [
        ([0, 0, 8, 8], 1),
        ([2, 2, 5, 5], 0),
        ([5, 2, 6, 3], 1),
        ([1, 3, 3, 4], 1),
        ([20, 20, 30, 30], 0),
        ([6, 6, 1, 1], 1),
        ([5, 5, 2, 2], 0),
        ([1, 1, 1, 5], 1),
        ([1, 1, 5, 1], 1),
        ([8, 0, 9, 1], 0),
        ([1, 1, 1, 1], 1),
        ([3, 3, 3, 3], 0),
        ([8, 1, 8, 1], 0),
        ([i32::MAX, i32::MAX, i32::MAX, i32::MAX], 0),
    ] {
        rect(&mut m, data, r);
        crate::util::set_last_error(&mut env(&mut m), 0x12345678);
        assert_eq!(
            call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]),
            expected,
            "{r:?}"
        );
        assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
    }
    assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!ExcludeClipRect", &[dc, 0, 0, 8, 8]),
        1
    );
    rect(&mut m, data, [0, 0, 8, 8]);
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]), 0);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
        1
    );
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]), 1);
    // A band begins in the hole but a later exclusion boundary becomes visible.
    rect(&mut m, data, [2, 2, 5, 6]);
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]), 1);
    let guarded = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
    rect(&mut m, guarded, [1, 1, 1, 1]);
    m.cpu.protect(guarded, 4096, Prot::READ).unwrap();
    m.cpu.protect(guarded + 4096, 4096, Prot(0)).unwrap();
    assert_eq!(
        call(&mut m, data, "gdi32.dll!RectVisible", &[dc, guarded]),
        1
    );
    for pointer in [0, 0xfffffff8, guarded + 4096 - 8] {
        assert_eq!(
            call(&mut m, data, "gdi32.dll!RectVisible", &[dc, pointer]),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut m)), 87);
    }
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[0, 0]), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 6);
    let gdi = m.ext.get::<Win32>().gdi.clone();
    if let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut m.sys.objects, dc) {
        modes.map = 2;
    }
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    if let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut m.sys.objects, dc) {
        modes.map = 1;
    }
    // Geometric visibility does not require permission to read/write the pixels.
    m.cpu.protect(pixels, 4096, Prot(0)).unwrap();
    super::super::super::front::install(&mut m);
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]), 1);
    m.cpu.protect(pixels, 4096, Prot::RW).unwrap();
    let mut raw = [0; 256];
    m.cpu.read(pixels, &mut raw).unwrap();
    assert_eq!(raw, [0xa5; 256]);
    assert_eq!(m.sys.objects.object_count(), count);
    assert!(super::super::super::clip::snapshot(&env(&mut m), dc).contains(0, 0));
    assert!(!super::super::super::clip::snapshot(&env(&mut m), dc).contains(3, 3));
    assert!(m.ext.get_mut::<SurfaceStore>().delete(16));
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    assert!(
        m.ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, _)| api == "gdi32.dll!RectVisible")
    );
}

#[test]
fn window_origin_query_reads_shared_modes_and_atomically_preserves_errors_and_lifetime() {
    let (mut m, data, dc, pixels) = fixture(8, 8, PixelFormat::Bgra32, false);
    let count = m.sys.objects.object_count();
    m.cpu.write(pixels, &[0xa5; 256]).unwrap();
    let point = data + 128;
    let value = |m: &Machine<JitCpu>| {
        [
            m.cpu.read_u32(point).unwrap() as i32,
            m.cpu.read_u32(point + 4).unwrap() as i32,
        ]
    };
    m.cpu.write(point - 4, &[0xa5; 16]).unwrap();
    crate::util::set_last_error(&mut env(&mut m), 0x12345678);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetWindowOrgEx", &[dc, point]),
        1
    );
    assert_eq!(value(&m), [0, 0]);
    assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
    assert_eq!(m.cpu.read_u32(point - 4).unwrap(), 0xa5a5a5a5);
    assert_eq!(m.cpu.read_u32(point + 8).unwrap(), 0xa5a5a5a5);
    let mut gdi = m.ext.get::<Win32>().gdi.clone();
    // Seed the common owner's modes, not an exported, unimplemented setter.
    let set_origin = |m: &mut Machine<JitCpu>, origin| {
        let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut m.sys.objects, dc) else {
            panic!("DC owner")
        };
        modes.window_origin = origin;
    };
    set_origin(&mut m, [11, -7]);
    assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
    set_origin(&mut m, [-2, 5]);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetWindowOrgEx", &[dc, point]),
        1
    );
    assert_eq!(value(&m), [-2, 5]);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
        1
    );
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetWindowOrgEx", &[dc, point]),
        1
    );
    assert_eq!(value(&m), [11, -7]);
    let short = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
    m.cpu.write_u32(short + 4092, 0xdeadbeef).unwrap();
    m.cpu.protect(short + 4096, 4096, Prot(0)).unwrap();
    for output in [0, 0xfffffffc, short + 4092] {
        assert_eq!(
            call(&mut m, data, "gdi32.dll!GetWindowOrgEx", &[dc, output]),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut m)), 87);
    }
    assert_eq!(m.cpu.read_u32(short + 4092).unwrap(), 0xdeadbeef);
    m.cpu.protect(short, 4096, Prot::READ).unwrap();
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetWindowOrgEx", &[dc, short]),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 87);
    crate::util::set_last_error(&mut env(&mut m), 0x12345678);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetWindowOrgEx", &[0, point]),
        0
    );
    assert_eq!(value(&m), [11, -7]);
    assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
    if let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut m.sys.objects, dc) {
        modes.map = 2;
    }
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetWindowOrgEx", &[dc, point]),
        0
    );
    assert_eq!(value(&m), [11, -7]);
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    if let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut m.sys.objects, dc) {
        modes.map = 1;
    }
    // This logical getter remains available without raster pixels/provider.
    m.cpu.protect(pixels, 4096, Prot(0)).unwrap();
    super::super::super::front::install(&mut m);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetWindowOrgEx", &[dc, point]),
        1
    );
    assert_eq!(value(&m), [11, -7]);
    m.cpu.protect(pixels, 4096, Prot::RW).unwrap();
    let mut raw = [0; 256];
    m.cpu.read(pixels, &mut raw).unwrap();
    assert_eq!(raw, [0xa5; 256]);
    assert_eq!(m.sys.objects.object_count(), count);
    assert!(
        m.ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, _)| api == "gdi32.dll!GetWindowOrgEx")
    );
    assert!(gdi.remove_external_dc(&mut m.sys.objects, dc));
    crate::util::set_last_error(&mut env(&mut m), 0x12345678);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetWindowOrgEx", &[dc, point]),
        0
    );
    assert_eq!(value(&m), [11, -7]);
    assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
}

#[test]
fn mm_text_origins_translate_pixels_and_each_transfer_owner_without_relocating_device_clip() {
    let (mut m, data, dc, pixels) = fixture(8, 8, PixelFormat::Bgra32, false);
    let count = m.sys.objects.object_count();
    let old = data + 128;
    assert_eq!(
        call(&mut m, data, "gdi32.dll!ExcludeClipRect", &[dc, 2, 2, 5, 5]),
        3
    );
    assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
    crate::util::set_last_error(&mut env(&mut m), 0x12345678);
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!SetWindowOrgEx",
            &[dc, (-1_i32) as u32, (-1_i32) as u32, old]
        ),
        1
    );
    assert_eq!(m.cpu.read_u32(old).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(old + 4).unwrap(), 0);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetViewportOrgEx", &[dc, 1, 0, old]),
        1
    );
    assert_eq!(m.cpu.read_u32(old).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(old + 4).unwrap(), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
    rect(&mut m, data, [0, 0, 5, 5]);
    assert_eq!(
        call(
            &mut m,
            data,
            "user32.dll!FillRect",
            &[dc, data, stock_object(0)]
        ),
        1
    );
    assert_eq!(masks(&m, pixels), [0, 124, 96, 96, 96, 124, 0, 0]);
    rect(&mut m, data, [0, 1, 3, 4]);
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]), 0);
    rect(&mut m, data, [3, 1, 4, 2]);
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]), 1);
    // New clipping coordinates translate now; the old hole remains device [2,2,5,5].
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!ExcludeClipRect",
            &[dc, (-2_i32) as u32, (-1_i32) as u32, u32::MAX, 0]
        ),
        3
    );
    let clip = super::super::super::clip::snapshot(&env(&mut m), dc);
    assert!(!clip.contains(0, 0));
    assert!(!clip.contains(3, 3));
    assert!(clip.contains(5, 3));
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetPixel", &[dc, 0, 1, 0xffffff]),
        u32::MAX
    );
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetPixel", &[dc, 0, 0, 0xffffff]),
        0xffffff
    );
    assert_eq!(m.cpu.read_u32(pixels + (8 + 2) * 4).unwrap(), 0xffffff);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetPixel", &[dc, 0, 0]),
        0xffffff
    );
    m.cpu.write(pixels, &[0; 256]).unwrap();
    rect(&mut m, data, [0, 0, 5, 5]);
    assert_eq!(
        call(
            &mut m,
            data,
            "user32.dll!FrameRect",
            &[dc, data, stock_object(0)]
        ),
        1
    );
    assert_eq!(masks(&m, pixels), [0, 124, 64, 64, 64, 124, 0, 0]);
    m.cpu.write(pixels, &[0; 256]).unwrap();
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!PatBlt",
            &[dc, 3, 2, 2, 2, 0xff0062]
        ),
        1
    );
    assert_eq!(masks(&m, pixels), [0, 0, 0, 96, 96, 0, 0, 0]);
    // Source and destination DCs have distinct origins; source clip is not applied.
    let mut contexts = adv_os::gdi::DeviceContexts::new(m.ext.get::<Win32>().gdi.clone());
    let (src, src_dc, src_pixels) = {
        let store = m.ext.get_mut::<SurfaceStore>();
        let src = store
            .create(8, 8, false, PixelFormat::Bgra32, &mut memory(&mut m.cpu))
            .unwrap();
        let src_dc = store.new_id();
        (src, src_dc, store.get(src).unwrap().pixels)
    };
    assert!(contexts.create(&mut m.sys.objects, src_dc, src));
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetWindowOrgEx", &[src_dc, 1, 0, 0]),
        1
    );
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!SetViewportOrgEx",
            &[src_dc, 3, 0, 0]
        ),
        1
    );
    for x in 0..8 {
        m.cpu.write_u32(src_pixels + x * 4, 0x10 + x).unwrap();
    }
    m.cpu.write(pixels, &[0; 256]).unwrap();
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!BitBlt",
            &[dc, 1, 0, 2, 1, src_dc, 0, 0, 0xcc0020]
        ),
        1
    );
    assert_eq!(m.cpu.read_u32(pixels + (8 + 3) * 4).unwrap(), 0x12);
    assert_eq!(m.cpu.read_u32(pixels + (8 + 4) * 4).unwrap(), 0x13);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetStretchBltMode", &[dc, 3]),
        1
    );
    m.cpu.write(pixels, &[0; 256]).unwrap();
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!StretchBlt",
            &[dc, 0, 0, 2, 1, src_dc, 0, 0, 4, 1, 0xcc0020]
        ),
        1
    );
    assert_eq!(m.cpu.read_u32(pixels + (8 + 2) * 4).unwrap(), 0x12);
    assert_eq!(m.cpu.read_u32(pixels + (8 + 3) * 4).unwrap(), 0x14);
    // DIB input coordinates are bitmap pixels, not an imaginary source DC origin.
    let info = data + 1024;
    let bits = data + 1100;
    let mut header = [0; 40];
    for (at, value) in [(0, 40_u32), (4, 2), (8, u32::MAX), (12, 0x0020_0001)] {
        header[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    m.cpu.write(info, &header).unwrap();
    m.cpu.write_u32(bits, 0x21).unwrap();
    m.cpu.write_u32(bits + 4, 0x22).unwrap();
    m.cpu.write(pixels, &[0; 256]).unwrap();
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!StretchDIBits",
            &[dc, 0, 0, 2, 1, 0, 0, 2, 1, bits, info, 0, 0xcc0020]
        ),
        1
    );
    assert_eq!(m.cpu.read_u32(pixels + (8 + 2) * 4).unwrap(), 0x21);
    assert_eq!(m.cpu.read_u32(pixels + (8 + 3) * 4).unwrap(), 0x22);
    // Count-zero opaque text checks the same translated rectangle + fixed clip,
    // without a host-dependent glyph shape expectation.
    m.cpu.write(pixels, &[0; 256]).unwrap();
    rect(&mut m, data, [0, 0, 2, 2]);
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!ExtTextOutW",
            &[dc, 0, 0, 2, data, 0, 0, 0]
        ),
        1
    );
    assert_eq!(masks(&m, pixels), [0, 12, 0, 0, 0, 0, 0, 0]);
    assert_eq!(call(&mut m, data, "gdi32.dll!DeleteDC", &[src_dc]), 1);
    assert_eq!(call(&mut m, data, "gdi32.dll!DeleteObject", &[src]), 1);
    // Failed optional previous POINT must change neither half-output nor origins.
    let guarded = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
    m.cpu.write_u32(guarded + 4092, 0xdeadbeef).unwrap();
    m.cpu.protect(guarded + 4096, 4096, Prot(0)).unwrap();
    let gdi = m.ext.get::<Win32>().gdi.clone();
    let modes = |m: &Machine<JitCpu>| {
        let Some(GdiObj::Dc { modes, .. }) = gdi.get(&m.sys.objects, dc) else {
            panic!("DC")
        };
        (modes.window_origin, modes.viewport_origin)
    };
    for api in ["gdi32.dll!SetWindowOrgEx", "gdi32.dll!SetViewportOrgEx"] {
        assert_eq!(call(&mut m, data, api, &[dc, 123, 456, guarded + 4092]), 0);
        assert_eq!(modes(&m), ([-1, -1], [1, 0]));
        assert_eq!(m.cpu.read_u32(guarded + 4092).unwrap(), 0xdeadbeef);
        assert_eq!(crate::util::last_error(&env(&mut m)), 87);
        assert_eq!(call(&mut m, data, api, &[dc, 123, 456, 0xfffffffc]), 0);
        assert_eq!(modes(&m), ([-1, -1], [1, 0]));
    }
    m.cpu.protect(guarded, 4096, Prot::READ).unwrap();
    for api in ["gdi32.dll!SetWindowOrgEx", "gdi32.dll!SetViewportOrgEx"] {
        assert_eq!(call(&mut m, data, api, &[dc, 1, 2, guarded]), 0);
        assert_eq!(modes(&m), ([-1, -1], [1, 0]));
        assert_eq!(crate::util::last_error(&env(&mut m)), 87);
        assert_eq!(call(&mut m, data, api, &[0, 1, 2, old]), 0);
        assert_eq!(crate::util::last_error(&env(&mut m)), 6);
    }
    if let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut m.sys.objects, dc) {
        modes.map = 2;
    }
    for api in ["gdi32.dll!SetWindowOrgEx", "gdi32.dll!SetViewportOrgEx"] {
        assert_eq!(call(&mut m, data, api, &[dc, 1, 2, old]), 0);
        assert_eq!(modes(&m), ([-1, -1], [1, 0]));
        assert_eq!(crate::util::last_error(&env(&mut m)), 50);
        assert!(
            m.ext
                .get::<adv_abi::ApiDiagnostics>()
                .warnings
                .keys()
                .any(|(name, _)| name == api)
        );
    }
    assert_eq!(
        call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
        1
    );
    assert_eq!(modes(&m), ([0, 0], [0, 0]));
    assert!(super::super::super::clip::snapshot(&env(&mut m), dc).contains(0, 0));
    // The actual request and nullable output use the same state, not a facade.
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!SetWindowOrgEx",
            &[dc, (-452_i32) as u32, (-18_i32) as u32, 0]
        ),
        1
    );
    assert_eq!(modes(&m), ([-452, -18], [0, 0]));
    assert_eq!(
        call(&mut m, data, "gdi32.dll!GetWindowOrgEx", &[dc, old]),
        1
    );
    assert_eq!(m.cpu.read_u32(old).unwrap() as i32, -452);
    assert_eq!(m.cpu.read_u32(old + 4).unwrap() as i32, -18);
    assert_eq!(m.sys.objects.object_count(), count);
}

#[test]
fn intersect_clip_rect_native_masks_and_device_origin_save_restore_failures_share_one_owner() {
    let (mut m, data, dc, pixels) = fixture(8, 8, PixelFormat::Bgra32, false);
    let count = m.sys.objects.object_count();
    for (r, expected, visible) in [
        ([2, 2, 5, 5], [0, 0, 28, 28, 28, 0, 0, 0], 1),
        ([20, 20, 30, 30], [0; 8], 0),
        ([0, 0, 8, 8], [255; 8], 1),
        ([5, 5, 2, 2], [0, 0, 28, 28, 28, 0, 0, 0], 1),
        ([2, 2, 2, 5], [0; 8], 0),
    ] {
        assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
        m.cpu.write(pixels, &[0; 256]).unwrap();
        crate::util::set_last_error(&mut env(&mut m), 0x12345678);
        assert_eq!(
            call(
                &mut m,
                data,
                "gdi32.dll!IntersectClipRect",
                &[dc, r[0] as u32, r[1] as u32, r[2] as u32, r[3] as u32]
            ),
            3
        );
        assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
        rect(&mut m, data, [0, 0, 8, 8]);
        assert_eq!(
            call(
                &mut m,
                data,
                "user32.dll!FillRect",
                &[dc, data, stock_object(0)]
            ),
            1
        );
        assert_eq!(masks(&m, pixels), expected);
        assert_eq!(
            call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]),
            visible
        );
        assert_eq!(
            call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
            1
        );
        assert!(super::super::super::clip::snapshot(&env(&mut m), dc).is_default());
    }
    assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!ExcludeClipRect", &[dc, 3, 3, 5, 5]),
        3
    );
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!IntersectClipRect",
            &[dc, 1, 1, 7, 7]
        ),
        3
    );
    m.cpu.write(pixels, &[0; 256]).unwrap();
    rect(&mut m, data, [0, 0, 8, 8]);
    assert_eq!(
        call(
            &mut m,
            data,
            "user32.dll!FillRect",
            &[dc, data, stock_object(0)]
        ),
        1
    );
    assert_eq!(masks(&m, pixels), [0, 126, 126, 102, 102, 126, 126, 0]);
    // Intersecting entirely with an excluded hole empties the real mask, while
    // this tested memory-DC API still reports its observed success value 3.
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!IntersectClipRect",
            &[dc, 3, 3, 5, 5]
        ),
        3
    );
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]), 0);
    let before = masks(&m, pixels);
    assert_eq!(
        call(
            &mut m,
            data,
            "user32.dll!FillRect",
            &[dc, data, stock_object(4)]
        ),
        1
    );
    assert_eq!(masks(&m, pixels), before);
    // A later, larger intersection never resurrects an empty clipping region.
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!IntersectClipRect",
            &[dc, 0, 0, 8, 8]
        ),
        3
    );
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]), 0);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
        1
    );
    assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!SetWindowOrgEx",
            &[dc, u32::MAX, u32::MAX, 0]
        ),
        1
    );
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetViewportOrgEx", &[dc, 1, 0, 0]),
        1
    );
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!IntersectClipRect",
            &[dc, 0, 0, 3, 4]
        ),
        3
    );
    m.cpu.write(pixels, &[0; 256]).unwrap();
    rect(&mut m, data, [0, 0, 8, 8]);
    assert_eq!(
        call(
            &mut m,
            data,
            "user32.dll!FillRect",
            &[dc, data, stock_object(0)]
        ),
        1
    );
    assert_eq!(masks(&m, pixels), [0, 28, 28, 28, 28, 0, 0, 0]);
    let clip = super::super::super::clip::snapshot(&env(&mut m), dc);
    assert!(clip.visible(8, 8)); // Requires the intersection top-band event.
    assert!(!clip.contains(2, 0));
    assert!(clip.contains(2, 1));
    assert!(!clip.contains(5, 1));
    assert!(!clip.contains(2, 5));
    // Origins move logical inputs, not the stored device intersection.
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetWindowOrgEx", &[dc, 0, 0, 0]),
        1
    );
    assert_eq!(call(&mut m, data, "gdi32.dll!RectVisible", &[dc, data]), 1);
    m.cpu.write(pixels, &[0; 256]).unwrap();
    assert_eq!(
        call(
            &mut m,
            data,
            "user32.dll!FillRect",
            &[dc, data, stock_object(0)]
        ),
        1
    );
    assert_eq!(masks(&m, pixels), [0, 28, 28, 28, 28, 0, 0, 0]);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
        1
    );
    let gdi = m.ext.get::<Win32>().gdi.clone();
    let snapshot = |m: &mut Machine<JitCpu>| super::super::super::clip::snapshot(&env(m), dc);
    let expected = snapshot(&mut m).spans(0, 8);
    if let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut m.sys.objects, dc) {
        modes.map = 2;
    }
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!IntersectClipRect",
            &[dc, 0, 0, 1, 1]
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    assert_eq!(snapshot(&mut m).spans(0, 8), expected);
    if let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut m.sys.objects, dc) {
        modes.map = 1;
    }
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!SetWindowOrgEx",
            &[dc, i32::MIN as u32, 0, 0]
        ),
        1
    );
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!SetViewportOrgEx",
            &[dc, i32::MAX as u32, 0, 0]
        ),
        1
    );
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!IntersectClipRect",
            &[dc, 1, 0, 2, 1]
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    assert!(snapshot(&mut m).is_default());
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!IntersectClipRect",
            &[0, 0, 0, 1, 1]
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 6);
    super::super::super::front::install(&mut m);
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!IntersectClipRect",
            &[dc, 0, 0, 1, 1]
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    assert!(snapshot(&mut m).is_default());
    assert_eq!(m.sys.objects.object_count(), count);
    assert!(
        m.ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(name, _)| name == "gdi32.dll!IntersectClipRect")
    );
}
