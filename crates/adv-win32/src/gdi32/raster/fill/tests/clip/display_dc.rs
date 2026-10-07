//! Authored display metadata borrows never replace another live window canvas.
use super::*;
use crate::user32::window::{Rect, WS_CHILD, WS_POPUP, WS_VISIBLE, Window};

const ROOT: u32 = 0x10020;
const OTHER: u32 = 0x10040;
const CHILD: u32 = 0x10060;

fn windows(m: &mut Machine<JitCpu>, own: bool) {
    m.ext.get_or_default::<adv_abi::ApiDiagnostics>();
    let user = &mut m.ext.get_mut::<Win32>().user;
    user.screen = (16, 12);
    for (hwnd, parent, width, height) in [(ROOT, 0, 8, 8), (OTHER, 0, 12, 12), (CHILD, ROOT, 4, 4)]
    {
        let mut window = Window::new(
            hwnd,
            0,
            0,
            WS_VISIBLE | if parent == 0 { WS_POPUP } else { WS_CHILD },
            0,
            parent,
            Rect::new(0, 0, width, height),
            0,
        );
        if own && hwnd == ROOT {
            window.dc_style = 0x20;
        }
        assert!(user.windows.insert(&mut m.sys.objects, hwnd, window));
    }
}

fn get(m: &mut Machine<JitCpu>, data: u32, hwnd: u32) -> u32 {
    let dc = call(m, data, "user32.dll!GetDC", &[hwnd]);
    assert_ne!(dc, 0);
    dc
}

fn backing(m: &Machine<JitCpu>, dc: u32) -> u32 {
    match m.ext.get::<Win32>().gdi.get(&m.sys.objects, dc).unwrap() {
        GdiObj::Dc { selected, .. } => selected[3],
        _ => unreachable!(),
    }
}

#[test]
fn display_dc_parallel_font_borrows_keep_the_live_canvas_and_independent_state() {
    let (mut m, data, _, _) = fixture(8, 8, PixelFormat::Bgra32, false);
    windows(&mut m, false);
    let count = m.sys.objects.object_count();
    let a = get(&mut m, data, ROOT);
    let primary = backing(&m, a);
    let pixels = m.ext.get::<SurfaceStore>().get(primary).unwrap().pixels;
    m.cpu.write(pixels, &[0xa5; 256]).unwrap();
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetTextColor", &[a, 0x332211]),
        0
    );
    assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[a]), 1);
    let b = get(&mut m, data, ROOT);
    let screen = get(&mut m, data, 0);
    let other = get(&mut m, data, OTHER);
    let child = get(&mut m, data, CHILD);
    assert_eq!(
        [a, b, screen, other, child]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        5
    );
    assert_eq!(backing(&m, b), primary);
    assert_eq!(
        [backing(&m, screen), backing(&m, other), backing(&m, child)],
        [0; 3]
    );
    for dc in [b, screen, other, child] {
        assert_eq!(call(&mut m, data, "gdi32.dll!SetTextColor", &[dc, 0]), 0);
        assert_eq!(
            call(&mut m, data, "gdi32.dll!GetCurrentObject", &[dc, 6]),
            stock_object(13)
        );
        assert_eq!(call(&mut m, data, "gdi32.dll!GetDeviceCaps", &[dc, 88]), 96);
    }
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetTextColor", &[a, 0xabcdef]),
        0x332211
    );
    assert_eq!(call(&mut m, data, "gdi32.dll!RestoreDC", &[a, 1]), 1);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetTextColor", &[a, 0]),
        0x332211
    );
    assert_eq!(call(&mut m, data, "gdi32.dll!SaveDC", &[screen]), 1);
    assert_eq!(call(&mut m, data, "gdi32.dll!RestoreDC", &[screen, 1]), 1);
    assert_eq!(backing(&m, screen), 0);
    let compatible = call(&mut m, data, "gdi32.dll!CreateCompatibleDC", &[screen]);
    assert_ne!(compatible, 0);
    assert_eq!(call(&mut m, data, "gdi32.dll!DeleteDC", &[compatible]), 1);
    for dc in [screen, other, child] {
        assert_eq!(
            call(&mut m, data, "gdi32.dll!SetPixel", &[dc, 0, 0, 0x123456]),
            u32::MAX
        );
        assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    }
    let surface = m.ext.get::<SurfaceStore>().get(primary).unwrap();
    assert_eq!(
        (surface.width, surface.height, surface.pixels),
        (8, 8, pixels)
    );
    let mut bytes = [0; 256];
    m.cpu.read(pixels, &mut bytes).unwrap();
    assert_eq!(bytes, [0xa5; 256]);
    assert_eq!(
        call(&mut m, data, "gdi32.dll!SetPixel", &[a, 1, 1, 0x123456]),
        0x123456
    );
    assert_eq!(m.ext.get::<SurfaceStore>().stats.presentations, 1);
    for (hwnd, dc) in [
        (0, screen),
        (OTHER, other),
        (CHILD, child),
        (ROOT, b),
        (ROOT, a),
    ] {
        assert_eq!(call(&mut m, data, "gdi32.dll!DeleteDC", &[dc]), 0);
        assert_eq!(call(&mut m, data, "user32.dll!ReleaseDC", &[hwnd, dc]), 1);
        assert_eq!(call(&mut m, data, "user32.dll!ReleaseDC", &[hwnd, dc]), 0);
    }
    let screen = get(&mut m, data, 0);
    assert_eq!(backing(&m, screen), primary);
    let surface = m.ext.get::<SurfaceStore>().get(primary).unwrap();
    assert_eq!((surface.width, surface.height), (16, 12));
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!SetPixel",
            &[screen, 10, 10, 0x654321]
        ),
        0x654321
    );
    assert_eq!(call(&mut m, data, "user32.dll!ReleaseDC", &[0, screen]), 1);
    assert_eq!(m.sys.objects.object_count(), count);
}

#[test]
fn display_dc_nested_paint_has_default_state_and_own_dc_retains_its_owner() {
    for own in [false, true] {
        let (mut m, data, _, _) = fixture(8, 8, PixelFormat::Bgra32, false);
        windows(&mut m, own);
        let a = call(&mut m, data, "user32.dll!BeginPaint", &[ROOT, data + 128]);
        assert_ne!(a, 0);
        assert_eq!(
            call(&mut m, data, "gdi32.dll!SetTextColor", &[a, 0x332211]),
            0
        );
        let b = call(&mut m, data, "user32.dll!BeginPaint", &[ROOT, data + 256]);
        assert_ne!(b, 0);
        assert_eq!(a == b, own);
        assert_eq!(
            call(&mut m, data, "gdi32.dll!SetTextColor", &[b, 0x123456]),
            if own { 0x332211 } else { 0 }
        );
        assert_eq!(
            call(&mut m, data, "user32.dll!EndPaint", &[ROOT, data + 256]),
            1
        );
        assert_eq!(
            call(&mut m, data, "user32.dll!EndPaint", &[ROOT, data + 128]),
            1
        );
        assert_eq!(
            m.ext.get::<Win32>().gdi.get(&m.sys.objects, a).is_some(),
            own
        );
        let c = get(&mut m, data, ROOT);
        assert_eq!(
            call(&mut m, data, "gdi32.dll!SetTextColor", &[c, 0]),
            if own { 0x123456 } else { 0 }
        );
        assert_eq!(call(&mut m, data, "user32.dll!ReleaseDC", &[ROOT, c]), 1);
        assert_eq!(
            call(&mut m, data, "user32.dll!ReleaseDC", &[ROOT, c]),
            u32::from(own)
        );
        super::super::super::super::front::retire_window(&mut env(&mut m), ROOT);
        assert!(m.ext.get::<Win32>().gdi.get(&m.sys.objects, c).is_none());
    }
}

#[test]
fn display_dc_common_leases_use_the_acquiring_thread_for_release() {
    let (mut m, data, _, _) = fixture(8, 8, PixelFormat::Bgra32, false);
    windows(&mut m, false);
    let a = get(&mut m, data, ROOT);
    let thread = m
        .spawn("borrower", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    m.sys.current = thread;
    let b = get(&mut m, data, ROOT);
    assert_ne!(a, b);
    assert_eq!(call(&mut m, data, "user32.dll!ReleaseDC", &[ROOT, a]), 0);
    assert_eq!(call(&mut m, data, "user32.dll!ReleaseDC", &[ROOT, b]), 1);
    m.sys.current = 0;
    assert_eq!(call(&mut m, data, "user32.dll!ReleaseDC", &[ROOT, a]), 1);
}
