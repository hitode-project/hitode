// Run explicitly on an interactive Windows desktop, with --test-threads=1.
// Captures below read this test's HWND client DC, not DWM or monitor scanout.
use super::*;
use std::time::Instant;
use windows_sys::Win32::System::Threading::{GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources};

struct ClientDc {
    hwnd: HWND,
    dc: HDC,
}

impl ClientDc {
    fn acquire(hwnd: HWND) -> Self {
        // SAFETY: the test owns a live HWND on this UI thread.
        let dc = unsafe { GetDC(hwnd) };
        assert!(!dc.is_null(), "GetDC failed: {}", unsafe { GetLastError() });
        Self { hwnd, dc }
    }
}

impl Drop for ClientDc {
    fn drop(&mut self) {
        // SAFETY: paired with GetDC on this still-live window.
        unsafe { ReleaseDC(self.hwnd, self.dc) };
    }
}

fn client_rect(hwnd: HWND) -> RECT {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    assert_ne!(unsafe { GetClientRect(hwnd, &mut rect) }, 0);
    assert!(rect.right > 0 && rect.bottom > 0);
    rect
}

fn pattern(serial: u64) -> Snapshot {
    let colors = [
        [24, 160, 240, 255],
        [210, 50, 150, 255],
        [60, 220, 80, 255],
        [200, 130, 40, 255],
    ];
    let mut frame = Snapshot {
        width: 384,
        height: 256,
        bgra: vec![0; 384 * 256 * 4],
        frames: serial,
    };
    for y in 0..256usize {
        for x in 0..384usize {
            let quadrant = usize::from(x >= 192) + 2 * usize::from(y >= 128);
            let color = colors[(quadrant + serial as usize % 4) % 4];
            let offset = (y * 384 + x) * 4;
            frame.bgra[offset..offset + 4].copy_from_slice(&color);
        }
    }
    frame
}

#[derive(Default)]
struct HostCapture {
    buffer: Option<Dib>,
    captures: u64,
    interior_pixels: u64,
    black_bar_pixels: u64,
}

impl HostCapture {
    fn check(&mut self, window: &Window, expected: &Snapshot, stage: &str) {
        let rect = client_rect(window.hwnd);
        if self
            .buffer
            .as_ref()
            .is_none_or(|dib| (dib.width, dib.height) != (rect.right, rect.bottom))
        {
            self.buffer = Some(Dib::new(rect.right, rect.bottom).unwrap());
        }
        let capture = self.buffer.as_ref().unwrap();
        {
            let source = ClientDc::acquire(window.hwnd);
            // SAFETY: both DCs and the selected capture DIB belong to this thread.
            assert_ne!(
                unsafe {
                    BitBlt(
                        capture.dc,
                        0,
                        0,
                        capture.width,
                        capture.height,
                        source.dc,
                        0,
                        0,
                        SRCCOPY,
                    )
                },
                0,
                "host client capture failed at {stage}"
            );
            assert_ne!(unsafe { GdiFlush() }, 0);
        }
        // SAFETY: GDI has finished writing the owned top-down 32-bit capture DIB.
        let pixels = unsafe {
            std::slice::from_raw_parts(
                capture.bits,
                capture.width as usize * capture.height as usize * 4,
            )
        };
        let viewport = Viewport::fit(&rect, expected.width, expected.height);
        let mut bad = None;
        for y in 0..capture.height {
            for x in 0..capture.width {
                let offset = (y as usize * capture.width as usize + x as usize) * 4;
                let black = pixels[offset..offset + 3] == [0, 0, 0];
                let inside = x >= viewport.x
                    && x < viewport.x + viewport.width
                    && y >= viewport.y
                    && y < viewport.y + viewport.height;
                if inside {
                    self.interior_pixels += 1;
                } else {
                    self.black_bar_pixels += 1;
                }
                if black == inside && bad.is_none() {
                    bad = Some(format!(
                        "unexpected {} at ({x},{y})",
                        if black {
                            "black interior pixel"
                        } else {
                            "nonblack letterbox pixel"
                        }
                    ));
                }
            }
        }
        // Interior markers distinguish the current image from any other bright frame.
        for qy in 0..2 {
            for qx in 0..2 {
                let x = viewport.x + viewport.width * (2 * qx + 1) / 4;
                let y = viewport.y + viewport.height * (2 * qy + 1) / 4;
                let actual_offset = (y as usize * capture.width as usize + x as usize) * 4;
                let sx = expected.width as usize * (2 * qx as usize + 1) / 4;
                let sy = expected.height as usize * (2 * qy as usize + 1) / 4;
                let expected_offset = (sy * expected.width as usize + sx) * 4;
                if pixels[actual_offset..actual_offset + 3]
                    != expected.bgra[expected_offset..expected_offset + 3]
                    && bad.is_none()
                {
                    bad = Some(format!("wrong retained image at marker ({x},{y})"));
                }
            }
        }
        self.captures += 1;
        if let Some(reason) = bad {
            let artifact = save_failure(pixels, capture.width, capture.height, stage);
            panic!("{stage}: {reason}; HWND client capture: {artifact}");
        }
    }
}

fn save_failure(bgra: &[u8], width: i32, height: i32, stage: &str) -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/presenter-desktop");
    let path = directory.join(format!("failure-{stamp}-{stage}.png"));
    let save = || -> Result<(), Box<dyn std::error::Error>> {
        std::fs::create_dir_all(&directory)?;
        let mut rgba = bgra.to_vec();
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        let mut encoder =
            png::Encoder::new(std::fs::File::create(&path)?, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&rgba)?;
        Ok(())
    };
    match save() {
        Ok(()) => path.display().to_string(),
        Err(error) => format!("could not save capture: {error}"),
    }
}

fn repaint_now(hwnd: HWND) {
    repaint_with_erase(hwnd, false);
}

fn repaint_with_erase(hwnd: HWND, erase: bool) {
    assert_ne!(unsafe { InvalidateRect(hwnd, null(), i32::from(erase)) }, 0);
    assert_ne!(unsafe { UpdateWindow(hwnd) }, 0);
}

fn visible_test_window(title: &str) -> Window {
    let window = Window::new(title, 384, 256, true).unwrap();
    assert_ne!(
        unsafe {
            SetWindowPos(
                window.hwnd,
                HWND_TOPMOST,
                32,
                32,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        },
        0
    );
    window
}

fn warmup_presenter_resources() -> u32 {
    // Creating an empty hidden HWND does not initialize GDI's per-process
    // caches. Exercise the same source DIB, scaled composition, window DC,
    // capture DIB and pixel-read paths before choosing a leak-check baseline.
    let mut window = visible_test_window("ADV host presenter resource warmup (no audio)");
    let hwnd = window.hwnd;
    let frame = pattern(0);
    let mut capture = HostCapture::default();
    window.present(&frame).unwrap();
    window.pump();
    capture.check(&window, &frame, "warmup-native");
    {
        let dc = ClientDc::acquire(hwnd);
        assert_ne!(unsafe { GetPixel(dc.dc, 32, 32) }, 0);
    }
    set_client_size(hwnd, 480, 360).unwrap();
    window.present(&frame).unwrap();
    repaint_with_erase(hwnd, true);
    capture.check(&window, &frame, "warmup-scaled");
    window.pump();
    drop(capture);
    drop(window);
    assert_eq!(unsafe { IsWindow(hwnd) }, 0);
    assert_ne!(unsafe { GdiFlush() }, 0);
    unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) }
}

// spec: HOST-VIEW-2
#[test]
#[ignore = "interactive Windows desktop: visible topmost test HWND for about 8 seconds; no audio"]
fn native_desktop_repaint_retains_pixels_without_new_guest_frame() {
    // Two complete create/render/capture/drop rounds distinguish one-time GDI
    // initialization from a leak. Counts must then stay exactly stable rather
    // than allowing an arbitrary resource-growth threshold.
    // This fixture must run alone: counts include all process test threads.
    let first_warmup = warmup_presenter_resources();
    let gdi_before = warmup_presenter_resources();
    assert_eq!(
        first_warmup, gdi_before,
        "GDI count grew across complete warmup lifecycles"
    );
    let started = Instant::now();
    let mut window = visible_test_window("ADV host HWND repaint regression (no audio)");
    let hwnd = window.hwnd;
    let mut frame = pattern(0);
    window.present(&frame).unwrap();
    window.pump();
    let mut capture = HostCapture::default();
    capture.check(&window, &frame, "first-frame");

    // Deterministic old-fails/new-passes assertion: deliberately invalidate
    // existing HOST pixels while guest frame production remains frozen.
    {
        let dc = ClientDc::acquire(hwnd);
        assert_ne!(unsafe { PatBlt(dc.dc, 176, 112, 32, 32, BLACKNESS) }, 0);
        assert_ne!(unsafe { GdiFlush() }, 0);
        assert_eq!(
            unsafe { GetPixel(dc.dc, 180, 116) },
            0,
            "precondition: corrupt the host pixels, not the guest Snapshot"
        );
    }
    repaint_now(hwnd);
    // Do NOT pump Event::Repaint or call present before this capture. WM_PAINT
    // must repair the client synchronously, even with no new guest frame.
    capture.check(&window, &frame, "frozen-frame-repaint");
    // BeginPaint synchronously sends WM_ERASEBKGND for this invalidation.
    // The retained image must remain valid throughout that callback path too.
    repaint_with_erase(hwnd, true);
    capture.check(&window, &frame, "frozen-frame-erase-repaint");
    let mut erase_repaints = 1u64;

    set_client_size(hwnd, 480, 360).unwrap();
    repaint_now(hwnd);
    capture.check(&window, &frame, "resized-repaint");
    assert!(
        capture.black_bar_pixels > 0,
        "resize must exercise legal black bars"
    );

    unsafe { ShowWindow(hwnd, SW_SHOWMINNOACTIVE) };
    assert_ne!(unsafe { IsIconic(hwnd) }, 0);
    std::thread::sleep(Duration::from_millis(80));
    unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    assert_eq!(unsafe { IsIconic(hwnd) }, 0);
    repaint_now(hwnd);
    capture.check(&window, &frame, "restored-repaint");
    window.pump();

    let mut cycles = 0u64;
    while started.elapsed() < Duration::from_secs(8) {
        let next = pattern(cycles + 1);
        // Verify private composition does not clear the existing HWND surface.
        // This capture observes a presentation boundary, not just final pixels.
        window.state.retained.update(&next).unwrap();
        let rect = client_rect(hwnd);
        assert!(window.state.retained.prepare(&rect).unwrap().is_some());
        capture.check(&window, &frame, "private-composition");
        window.present(&next).unwrap();
        frame = next;
        capture.check(&window, &frame, "presented-frame");
        let erase = cycles.is_multiple_of(2);
        repaint_with_erase(hwnd, erase);
        erase_repaints += u64::from(erase);
        capture.check(&window, &frame, "continuous-repaint");
        assert!(
            window.state.paint_error.is_none(),
            "{:?}",
            window.state.paint_error
        );
        window.pump();
        cycles += 1;
        std::thread::sleep(Duration::from_millis(16));
    }
    assert!(cycles >= 30, "too few presentation cycles: {cycles}");
    let stats = (
        capture.captures,
        capture.interior_pixels,
        capture.black_bar_pixels,
    );
    drop(capture);
    drop(window); // DestroyWindow also removes this test HWND's topmost state.
    assert_eq!(unsafe { IsWindow(hwnd) }, 0, "test HWND must be destroyed");
    assert_ne!(unsafe { GdiFlush() }, 0);
    let gdi_after = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
    assert!(
        gdi_after <= gdi_before,
        "GDI resource leak: before={gdi_before}, after={gdi_after}; run with --test-threads=1"
    );
    println!(
        "HWND client-DC capture (not DWM/scanout): cycles={cycles}, erase_repaints={erase_repaints}, captures={}, \
        interior_pixels={}, legal_black_bar_pixels={}, extra_black_pixels=0, \
        elapsed={:.3}s, GDI_before={gdi_before}, GDI_after={gdi_after}, HWND_destroyed=true",
        stats.0,
        stats.1,
        stats.2,
        started.elapsed().as_secs_f64()
    );
}
