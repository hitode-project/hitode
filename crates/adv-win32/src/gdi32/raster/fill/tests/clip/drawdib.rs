//! Authored native DrawDib pixels through the common GDI test fixture.
use super::*;

const OPEN: &str = "msvfw32.dll!DrawDibOpen";
const CLOSE: &str = "msvfw32.dll!DrawDibClose";
const DRAW: &str = "msvfw32.dll!DrawDibDraw";

fn source(m: &mut Machine<JitCpu>, data: u32, bpp: u32) -> (u32, u32) {
    let info = data + 1024;
    let colors = if bpp == 8 { 16 } else { 0 };
    let bits = info + 40 + colors * 4;
    m.cpu.write(info, &[0; 40]).unwrap();
    for (at, word) in [(0, 40), (4, 4), (8, 4), (12, bpp << 16 | 1), (32, colors)] {
        m.cpu.write_u32(info + at, word).unwrap();
    }
    if bpp == 8 {
        let palette: Vec<_> = (0..16).flat_map(|i| [i + 1, i + 17, i + 33, 0]).collect();
        m.cpu.write(info + 40, &palette).unwrap();
    }
    let bytes: Vec<_> = (0..4)
        .rev()
        .flat_map(|y| {
            (0..4).flat_map(move |x| {
                let index = y * 4 + x;
                if bpp == 8 {
                    vec![index]
                } else {
                    vec![index + 1, index + 17, index + 33, 127]
                }
            })
        })
        .collect();
    m.cpu.write(bits, &bytes).unwrap();
    (info, bits)
}

fn draw(
    m: &mut Machine<JitCpu>,
    data: u32,
    handle: u32,
    dc: u32,
    dst: [i32; 4],
    src: [i32; 4],
    info: u32,
    bits: u32,
    flags: u32,
) -> u32 {
    let [x, y, w, h] = dst.map(|i| i as u32);
    let [sx, sy, sw, sh] = src.map(|i| i as u32);
    call(
        m,
        data,
        DRAW,
        &[handle, dc, x, y, w, h, info, bits, sx, sy, sw, sh, flags],
    )
}

#[test]
fn draw_dib_native_lifetime_is_owned_by_objects_and_rejects_foreign_handles() {
    let (mut m, data, dc, _) = fixture(6, 6, PixelFormat::Bgra32, false);
    let before = m.sys.objects.object_count();
    crate::util::set_last_error(&mut env(&mut m), 0x12345678);
    let a = call(&mut m, data, OPEN, &[]);
    let b = call(&mut m, data, OPEN, &[]);
    assert_ne!(a, 0);
    assert_ne!(a, b);
    assert_eq!(m.sys.objects.object_count(), before + 2);
    for invalid in [0, dc, 0xdeadbeef] {
        assert_eq!(call(&mut m, data, CLOSE, &[invalid]), 0);
    }
    assert_eq!(call(&mut m, data, CLOSE, &[a]), 1);
    assert_eq!(call(&mut m, data, CLOSE, &[a]), 0);
    assert_eq!(call(&mut m, data, CLOSE, &[b]), 1);
    assert_eq!(m.sys.objects.object_count(), before);
    assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
}

#[test]
fn draw_dib_native_palette_orientation_scaling_clip_and_default_size() {
    let (mut m, data, dc, pixels) = fixture(6, 6, PixelFormat::Bgra32, false);
    let handle = call(&mut m, data, OPEN, &[]);
    for bpp in [8, 32] {
        let (info, bits) = source(&mut m, data, bpp);
        for (dst, src, clipped, contiguous) in [
            ([0, 0, 4, 4], [0, 0, 4, 4], false, false),
            ([1, 1, 2, 1], [1, 0, 2, 1], false, false),
            ([1, 1, 2, 1], [1, 3, 2, 1], false, false),
            ([1, 1, 4, 4], [1, 1, 2, 2], false, false),
            ([0, 0, 4, 4], [0, 0, 4, 4], true, false),
            ([0, 0, -1, -1], [1, 1, 2, 2], false, false),
            ([0, 0, 2, 2], [0, 0, 4, 4], false, false),
            ([0, 0, 0, 4], [0, 0, 4, 4], false, false),
            ([0, 0, 4, 4], [0, 0, 4, 4], false, true),
        ] {
            m.cpu.write(pixels, &[0xa5; 144]).unwrap();
            assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[dc]), 1);
            if clipped {
                assert_eq!(
                    call(&mut m, data, "gdi32.dll!ExcludeClipRect", &[dc, 1, 1, 3, 3]),
                    3
                );
            }
            crate::util::set_last_error(&mut env(&mut m), 0x12345678);
            assert_eq!(
                draw(
                    &mut m,
                    data,
                    handle,
                    dc,
                    dst,
                    src,
                    info,
                    if contiguous { 0 } else { bits },
                    0
                ),
                1
            );
            assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
            assert_eq!(
                m.ext
                    .get::<Win32>()
                    .gdi
                    .clone()
                    .get(&m.sys.objects, dc)
                    .and_then(|o| match o {
                        GdiObj::Dc { modes, .. } => Some(modes.stretch),
                        _ => None,
                    }),
                Some(3)
            );
            let [sx, sy, sw, sh] = src;
            let [dx, dy, mut dw, mut dh] = dst;
            if dw == -1 {
                dw = sw;
            }
            if dh == -1 {
                dh = sh;
            }
            let mut actual = [0; 144];
            m.cpu.read(pixels, &mut actual).unwrap();
            for y in 0..6 {
                for x in 0..6 {
                    let expected = if x >= dx
                        && x < dx + dw
                        && y >= dy
                        && y < dy + dh
                        && !(clipped && (1..3).contains(&x) && (1..3).contains(&y))
                    {
                        let px = sx + ((2 * (x - dx) + 1) * sw) / (2 * dw);
                        let py = sy + ((2 * (y - dy) + 1) * sh) / (2 * dh);
                        let index = (py * 4 + px) as u8;
                        [
                            index + 1,
                            index + 17,
                            index + 33,
                            if bpp == 8 { 0 } else { 127 },
                        ]
                    } else {
                        [0xa5; 4]
                    };
                    let at = ((y * 6 + x) * 4) as usize;
                    assert_eq!(
                        &actual[at..at + 4],
                        &expected,
                        "bpp={bpp} dst={dst:?} src={src:?} pixel={x},{y}"
                    );
                }
            }
            assert_eq!(
                call(&mut m, data, "gdi32.dll!RestoreDC", &[dc, u32::MAX]),
                1
            );
        }
    }
    assert_eq!(call(&mut m, data, CLOSE, &[handle]), 1);
}

#[test]
fn draw_dib_invalid_inputs_preserve_pixels_and_capability_gaps_warn() {
    let (mut m, data, dc, pixels) = fixture(6, 6, PixelFormat::Bgra32, false);
    let handle = call(&mut m, data, OPEN, &[]);
    let (info, bits) = source(&mut m, data, 32);
    m.cpu.write(pixels, &[0xa5; 144]).unwrap();
    let rect = [0, 0, 4, 4];
    crate::util::set_last_error(&mut env(&mut m), 0x12345678);
    for (h, d, i, b, src) in [
        (0, dc, info, bits, rect),
        (handle, 0, info, bits, rect),
        (handle, dc, 0, bits, rect),
        (handle, dc, info, 1, rect),
        (handle, dc, info, bits, [0, 0, 0, 4]),
        (handle, dc, info, bits, [0, 3, 4, 2]),
    ] {
        assert_eq!(draw(&mut m, data, h, d, rect, src, i, b, 0), 0);
        assert_eq!(crate::util::last_error(&env(&mut m)), 0x12345678);
    }
    for (offset, value) in [(16, 1), (12, 24 << 16 | 1)] {
        let original = m.cpu.read_u32(info + offset).unwrap();
        m.cpu.write_u32(info + offset, value).unwrap();
        assert_eq!(draw(&mut m, data, handle, dc, rect, rect, info, bits, 0), 0);
        assert_eq!(crate::util::last_error(&env(&mut m)), 50);
        m.cpu.write_u32(info + offset, original).unwrap();
    }
    assert_eq!(draw(&mut m, data, handle, dc, rect, rect, info, bits, 2), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    let mut actual = [0; 144];
    m.cpu.read(pixels, &mut actual).unwrap();
    assert_eq!(actual, [0xa5; 144]);
    assert_eq!(
        m.ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .values()
            .sum::<u64>(),
        3
    );
    assert_eq!(call(&mut m, data, CLOSE, &[handle]), 1);
    assert_eq!(draw(&mut m, data, handle, dc, rect, rect, info, bits, 0), 0);
}
