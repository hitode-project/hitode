//! Native masks and typed argument transport over the same DC/surface owner.
use super::*;
use adv_cpu::backend::Reg;

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
