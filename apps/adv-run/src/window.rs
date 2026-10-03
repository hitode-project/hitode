use adv_runtime::Snapshot;
pub use pal_host::WakeEvent;
use std::time::Duration;

#[cfg_attr(not(windows), allow(dead_code))]
pub enum Event {
    Input {
        message: u32,
        wparam: u32,
        lparam: u32,
    },
    Close,
    FocusLost,
    Repaint,
}

#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum WaitOutcome {
    Input,
    Deadline,
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::collections::VecDeque;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, HANDLE, HWND, LPARAM, LRESULT, RECT, WAIT_OBJECT_0,
        WAIT_TIMEOUT, WPARAM,
    };
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateWaitableTimerExW,
        SetWaitableTimerEx, TIMER_ALL_ACCESS,
    };
    use windows_sys::Win32::UI::HiDpi::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    struct State {
        events: VecDeque<Event>,
        width: u32,
        height: u32,
        client_width: i32,
        client_height: i32,
        closed: bool,
    }
    pub struct Window {
        hwnd: HWND,
        state: Box<State>,
        timer: HANDLE,
        frame_size: Option<(u32, u32)>,
    }

    pub fn initialize_dpi() -> Result<(), String> {
        static RESULT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
        RESULT
            .get_or_init(|| {
                // Set this before runtime font/DC creation and the first HWND. PMv2
                // gives us physical pixels and prevents Windows bitmap stretching.
                unsafe {
                    if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
                        != 0
                        || AreDpiAwarenessContextsEqual(
                            GetThreadDpiAwarenessContext(),
                            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                        ) != 0
                    {
                        Ok(())
                    } else {
                        Err(format!(
                            "SetProcessDpiAwarenessContext failed: {}",
                            GetLastError()
                        ))
                    }
                }
            })
            .clone()
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Viewport {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    }
    impl Viewport {
        fn fit(client: &RECT, width: u32, height: u32) -> Self {
            let cw = (client.right - client.left).max(0);
            let ch = (client.bottom - client.top).max(0);
            if cw == 0 || ch == 0 {
                return Self {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                };
            }
            let (w, h) = if i64::from(cw) * i64::from(height) <= i64::from(ch) * i64::from(width) {
                (
                    cw,
                    (i64::from(cw) * i64::from(height) / i64::from(width)).max(1) as i32,
                )
            } else {
                (
                    (i64::from(ch) * i64::from(width) / i64::from(height)).max(1) as i32,
                    ch,
                )
            };
            Self {
                x: (cw - w) / 2,
                y: (ch - h) / 2,
                width: w,
                height: h,
            }
        }
        fn position(self, x: i32, y: i32, width: u32, height: u32) -> (i32, i32) {
            // Keep signed out-of-canvas positions during captured drags. Flooring
            // prevents a point just outside the viewport from becoming pixel 0.
            (
                (i64::from(x - self.x) * i64::from(width)).div_euclid(i64::from(self.width.max(1)))
                    as i32,
                (i64::from(y - self.y) * i64::from(height))
                    .div_euclid(i64::from(self.height.max(1))) as i32,
            )
        }
    }

    fn outer_rect(width: i32, height: i32, dpi: u32) -> Result<RECT, String> {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        // SAFETY: valid rectangle; the fixed window style has no menu.
        if unsafe { AdjustWindowRectExForDpi(&mut rect, WS_OVERLAPPEDWINDOW, 0, 0, dpi) } == 0 {
            return Err(format!("AdjustWindowRectExForDpi failed: {}", unsafe {
                GetLastError()
            }));
        }
        Ok(rect)
    }

    fn set_client_size(hwnd: HWND, width: u32, height: u32) -> Result<(), String> {
        let rect = outer_rect(width as i32, height as i32, unsafe {
            GetDpiForWindow(hwnd)
        })?;
        // SAFETY: the live, owned window stays at its chosen position/z-order.
        if unsafe {
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        } == 0
        {
            return Err(format!("SetWindowPos failed: {}", unsafe {
                GetLastError()
            }));
        }
        Ok(())
    }

    fn paint_frame(dc: HDC, client: &RECT, frame: &Snapshot) -> Result<(), String> {
        let viewport = Viewport::fit(client, frame.width, frame.height);
        if viewport.width == 0 || viewport.height == 0 {
            return Ok(());
        }
        // SAFETY: caller owns the DC, and Snapshot owns packed BGRA pixels.
        unsafe {
            PatBlt(dc, 0, 0, client.right, client.bottom, BLACKNESS);
            SetStretchBltMode(dc, COLORONCOLOR);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: frame.width as i32,
                    biHeight: -(frame.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                },
                ..std::mem::zeroed()
            };
            let result = StretchDIBits(
                dc,
                viewport.x,
                viewport.y,
                viewport.width,
                viewport.height,
                0,
                0,
                frame.width as i32,
                frame.height as i32,
                frame.bgra.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            if result == 0 || result == -1 {
                return Err("StretchDIBits failed".into());
            }
        }
        Ok(())
    }

    unsafe extern "system" fn procedure(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: CREATESTRUCT is provided by CreateWindowExW; state remains boxed until
        // after DestroyWindow. Callbacks and queue reads run on the same host thread.
        unsafe {
            if message == WM_NCCREATE {
                let create = &*(lparam as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
            if !pointer.is_null() {
                if message == WM_DPICHANGED {
                    let suggested = *(lparam as *const RECT);
                    // Preserve the chosen physical client size across monitors;
                    // only the native border/caption should follow the new DPI.
                    if let Ok(rect) = outer_rect(
                        (*pointer).client_width,
                        (*pointer).client_height,
                        (wparam as u32 & 0xffff).max(96),
                    ) {
                        SetWindowPos(
                            hwnd,
                            null_mut(),
                            suggested.left,
                            suggested.top,
                            rect.right - rect.left,
                            rect.bottom - rect.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                    (*pointer).events.push_back(Event::Repaint);
                    return 0;
                }
                let state = &mut *pointer;
                match message {
                    WM_SIZE => {
                        if wparam as u32 != SIZE_MINIMIZED {
                            state.client_width = lparam as u16 as i32;
                            state.client_height = (lparam as u32 >> 16) as u16 as i32;
                        }
                        state.events.push_back(Event::Repaint);
                        return 0;
                    }
                    WM_KILLFOCUS => {
                        state.events.push_back(Event::FocusLost);
                        return 0;
                    }
                    WM_CLOSE => {
                        if !state.closed {
                            state.events.push_back(Event::Close);
                            state.closed = true;
                        }
                        ShowWindow(hwnd, SW_HIDE);
                        return 0;
                    }
                    WM_ERASEBKGND => return 1,
                    WM_PAINT => {
                        let mut paint = std::mem::zeroed();
                        BeginPaint(hwnd, &mut paint);
                        EndPaint(hwnd, &paint);
                        state.events.push_back(Event::Repaint);
                        return 0;
                    }
                    WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
                        state.events.push_back(Event::Input {
                            message,
                            wparam: wparam as u32,
                            lparam: lparam as u32,
                        });
                        return 0;
                    }
                    // The ANSI guest's TranslateMessage generates its characters.
                    // Host UTF-16 character messages must not enter that queue.
                    WM_CHAR | WM_SYSCHAR => return 0,
                    WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN
                    | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP | WM_MOUSEWHEEL => {
                        let mut point = windows_sys::Win32::Foundation::POINT {
                            x: lparam as u16 as i16 as i32,
                            y: ((lparam as u32 >> 16) as u16) as i16 as i32,
                        };
                        if message == WM_MOUSEWHEEL {
                            ScreenToClient(hwnd, &mut point);
                        }
                        let mut rect: RECT = std::mem::zeroed();
                        GetClientRect(hwnd, &mut rect);
                        let viewport = Viewport::fit(&rect, state.width, state.height);
                        let (x, y) = viewport.position(point.x, point.y, state.width, state.height);
                        if (x < 0 || y < 0 || x >= state.width as i32 || y >= state.height as i32)
                            && matches!(
                                message,
                                WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_MOUSEWHEEL
                            )
                        {
                            return 0;
                        }
                        let position = u32::from(x as u16) | (u32::from(y as u16) << 16);
                        state.events.push_back(Event::Input {
                            message,
                            wparam: wparam as u32,
                            lparam: position,
                        });
                        if matches!(message, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN) {
                            SetCapture(hwnd);
                        }
                        // MK_LBUTTON | MK_RBUTTON | MK_MBUTTON: retain capture
                        // until every held button has been released.
                        if matches!(message, WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP)
                            && wparam & 0x13 == 0
                        {
                            ReleaseCapture();
                        }
                        return 0;
                    }
                    _ => {}
                }
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
    }

    impl Window {
        /// Align the next display opportunity to the compositor's QPC vblank
        /// phase. This timer is owned by UI and never wakes or blocks the guest.
        pub fn next_refresh(&self) -> std::time::Instant {
            use windows_sys::Win32::Graphics::Dwm::{DWM_TIMING_INFO, DwmGetCompositionTimingInfo};
            use windows_sys::Win32::System::Performance::{
                QueryPerformanceCounter, QueryPerformanceFrequency,
            };
            let mut timing = DWM_TIMING_INFO {
                cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
                ..Default::default()
            };
            let (mut counter, mut frequency) = (0i64, 0i64);
            let now = std::time::Instant::now();
            // The Windows 8.1+ API requires a null HWND, including windowed apps.
            if unsafe { DwmGetCompositionTimingInfo(null_mut(), &mut timing) } >= 0
                && unsafe { QueryPerformanceCounter(&mut counter) } != 0
                && unsafe { QueryPerformanceFrequency(&mut frequency) } != 0
                && frequency > 0
                && counter >= 0
                && timing.qpcRefreshPeriod > 0
            {
                let counter = counter as u64;
                let period = timing.qpcRefreshPeriod;
                let ticks = if timing.qpcVBlank > counter {
                    timing.qpcVBlank - counter
                } else {
                    period - (counter - timing.qpcVBlank) % period
                };
                let delay = Duration::from_secs_f64(ticks as f64 / frequency as f64);
                if delay <= Duration::from_millis(100) {
                    return now + delay;
                }
            }
            now + Duration::from_nanos(16_666_667)
        }
        pub fn new(title: &str, width: u32, height: u32, hidden: bool) -> Result<Self, String> {
            initialize_dpi()?;
            let class: Vec<u16> = "AdvGuestHost\0".encode_utf16().collect();
            let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
            let mut state = Box::new(State {
                events: VecDeque::new(),
                width,
                height,
                client_width: width as i32,
                client_height: height as i32,
                closed: false,
            });
            // SAFETY: null-terminated strings and the boxed state outlive window creation.
            let hwnd = unsafe {
                let instance = GetModuleHandleW(null());
                let definition = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(procedure),
                    hInstance: instance,
                    lpszClassName: class.as_ptr(),
                    hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                    ..std::mem::zeroed()
                };
                if RegisterClassW(&definition) == 0 && GetLastError() != 1410 {
                    return Err(format!("RegisterClassW failed: {}", GetLastError()));
                }
                let style = WS_OVERLAPPEDWINDOW;
                let rect = outer_rect(width as i32, height as i32, GetDpiForSystem())?;
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    title.as_ptr(),
                    style,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    null_mut(),
                    null_mut(),
                    instance,
                    (&mut *state as *mut State).cast(),
                );
                if hwnd.is_null() {
                    return Err(format!("CreateWindowExW failed: {}", GetLastError()));
                }
                // CW_USEDEFAULT may choose another monitor. Its actual DPI
                // determines the border needed for an exact physical client.
                if let Err(error) = set_client_size(hwnd, width, height) {
                    DestroyWindow(hwnd);
                    return Err(error);
                }
                if !hidden {
                    ShowWindow(hwnd, SW_SHOW);
                    UpdateWindow(hwnd);
                }
                hwnd
            };
            // SAFETY: this auto-reset timer has no callback and stays owned by Window.
            let timer = unsafe {
                CreateWaitableTimerExW(
                    null(),
                    null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS,
                )
            };
            if timer.is_null() {
                let error = unsafe { GetLastError() };
                unsafe {
                    DestroyWindow(hwnd);
                }
                return Err(format!("CreateWaitableTimerExW failed: {error}"));
            }
            Ok(Self {
                hwnd,
                state,
                timer,
                frame_size: None,
            })
        }

        pub fn pump(&mut self) -> Vec<Event> {
            // SAFETY: the Win32 message pump executes only on the window's owning thread.
            unsafe {
                let mut message = std::mem::zeroed();
                while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if message.message == WM_QUIT {
                        self.state.events.push_back(Event::Close);
                    } else {
                        DispatchMessageW(&message);
                    }
                }
            }
            self.state.events.drain(..).collect()
        }

        pub fn post_input(&mut self, message: u32, wparam: u32, lparam: u32) -> Result<(), String> {
            // SAFETY: queued delivery to our own window exercises the same host
            // message pump as physical input, without retaining any pointers.
            let posted =
                unsafe { PostMessageW(self.hwnd, message, wparam as usize, lparam as isize) };
            if posted == 0 {
                return Err(format!("PostMessageW failed: {}", unsafe {
                    GetLastError()
                }));
            }
            Ok(())
        }

        pub fn present(&mut self, frame: &Snapshot) -> Result<(), String> {
            if frame.width == 0
                || frame.height == 0
                || frame.bgra.len() != frame.width as usize * frame.height as usize * 4
            {
                return Err("invalid guest frame dimensions or pixels".into());
            }
            self.state.width = frame.width;
            self.state.height = frame.height;
            if self.frame_size != Some((frame.width, frame.height)) {
                set_client_size(self.hwnd, frame.width, frame.height)?;
                self.frame_size = Some((frame.width, frame.height));
            }
            // SAFETY: the pixel array has packed BGRA rows and is valid throughout the call.
            unsafe {
                let dc = GetDC(self.hwnd);
                if dc.is_null() {
                    return Err("GetDC failed".into());
                }
                let mut rect: RECT = std::mem::zeroed();
                GetClientRect(self.hwnd, &mut rect);
                // Aspect-preserving fit with explicit nearest pixel sampling.
                // Letterboxing never changes the guest canvas or input space.
                let result = paint_frame(dc, &rect, frame);
                ReleaseDC(self.hwnd, dc);
                result?;
            }
            Ok(())
        }

        #[allow(dead_code)] // Also used by the standalone wait_probe example.
        pub fn wait(&mut self, duration: Option<Duration>) -> Result<WaitOutcome, String> {
            self.wait_with_wake(duration, None)
        }

        pub fn hide(&mut self) {
            unsafe {
                ShowWindow(self.hwnd, SW_HIDE);
            }
        }

        pub fn wait_with_wake(
            &mut self,
            duration: Option<Duration>,
            wake: Option<&WakeEvent>,
        ) -> Result<WaitOutcome, String> {
            let timed = duration.is_some_and(|d| !d.is_zero());
            if let Some(duration) = duration.filter(|d| !d.is_zero()) {
                let ticks = duration.as_nanos().saturating_add(99) / 100;
                let due = -(ticks.min(i64::MAX as u128) as i64);
                // SAFETY: the negative relative deadline is copied synchronously;
                // no APC callback, wake context or retained pointer is supplied.
                if unsafe { SetWaitableTimerEx(self.timer, &due, 0, None, null(), null(), 0) } == 0
                {
                    return Err(format!("SetWaitableTimerEx failed: {}", unsafe {
                        GetLastError()
                    }));
                }
            }
            let timeout = if duration.is_some_and(|d| d.is_zero()) {
                0
            } else {
                u32::MAX
            };
            // SAFETY: an owned timer plus the host message queue are the only wake
            // sources. The timeout is never used to round a nonzero deadline.
            let mut handles = [null_mut(); 2];
            let mut count = 0;
            if timed {
                handles[count] = self.timer;
                count += 1;
            }
            if let Some(wake) = wake {
                handles[count] = wake.as_raw_handle();
                count += 1;
            }
            let result = unsafe {
                MsgWaitForMultipleObjectsEx(
                    count as u32,
                    if count == 0 { null() } else { handles.as_ptr() },
                    timeout,
                    QS_ALLINPUT,
                    MWMO_INPUTAVAILABLE,
                )
            };
            if result == WAIT_TIMEOUT || (timed && result == WAIT_OBJECT_0) {
                return Ok(WaitOutcome::Deadline);
            }
            let control_wake = wake.is_some() && result == WAIT_OBJECT_0 + u32::from(timed);
            if control_wake || result == WAIT_OBJECT_0 + count as u32 {
                if timed {
                    unsafe {
                        CancelWaitableTimer(self.timer);
                    }
                }
                return Ok(WaitOutcome::Input);
            }
            Err(format!("message wait failed ({result:#x}): {}", unsafe {
                GetLastError()
            }))
        }
    }

    impl Drop for Window {
        fn drop(&mut self) {
            // SAFETY: state remains live until synchronous destruction and callbacks finish.
            unsafe {
                CancelWaitableTimer(self.timer);
                CloseHandle(self.timer);
                DestroyWindow(self.hwnd);
            }
        }
    }

    #[cfg(test)]
    mod presentation_tests {
        use super::*;

        fn client(window: &Window) -> (i32, i32) {
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { GetClientRect(window.hwnd, &mut rect) }, 0);
            (rect.right, rect.bottom)
        }
        fn position(x: i32, y: i32) -> u32 {
            u32::from(x as u16) | (u32::from(y as u16) << 16)
        }

        #[test]
        fn native_frame_size_is_physical_and_resize_input_uses_the_letterbox() {
            let mut window = Window::new("physical pixel regression", 640, 480, true).unwrap();
            assert_ne!(
                unsafe {
                    AreDpiAwarenessContextsEqual(
                        GetWindowDpiAwarenessContext(window.hwnd),
                        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                    )
                },
                0
            );
            assert_eq!(client(&window), (640, 480));
            let mut frame = Snapshot {
                width: 800,
                height: 600,
                bgra: vec![255; 800 * 600 * 4],
                frames: 1,
            };
            window.present(&frame).unwrap();
            assert_eq!(
                client(&window),
                (800, 600),
                "guest mode change must use native physical pixels"
            );
            let dpi = unsafe { GetDpiForWindow(window.hwnd) };
            println!("PMv2 window DPI={dpi}; initial guest/client=800x600 physical pixels");
            let rect = outer_rect(800, 800, dpi).unwrap();
            assert_ne!(
                unsafe {
                    SetWindowPos(
                        window.hwnd,
                        null_mut(),
                        0,
                        0,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                },
                0
            );
            window.present(&frame).unwrap();
            assert_eq!(
                client(&window),
                (800, 800),
                "presentation must preserve a user resize"
            );
            window.pump();
            window
                .post_input(WM_LBUTTONDOWN, 1, position(400, 400))
                .unwrap();
            window
                .post_input(WM_MOUSEMOVE, 1, position(400, 99))
                .unwrap();
            window
                .post_input(WM_LBUTTONUP, 0, position(400, 50))
                .unwrap();
            window
                .post_input(WM_LBUTTONDOWN, 1, position(400, 50))
                .unwrap();
            let mouse: Vec<_> = window
                .pump()
                .into_iter()
                .filter_map(|event| match event {
                    Event::Input {
                        message, lparam, ..
                    } => Some((message, lparam)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                mouse,
                [
                    (WM_LBUTTONDOWN, position(400, 300)),
                    (WM_MOUSEMOVE, position(400, -1)),
                    (WM_LBUTTONUP, position(400, -50))
                ]
            );
            let mut wheel = windows_sys::Win32::Foundation::POINT { x: 400, y: 400 };
            assert_ne!(unsafe { ClientToScreen(window.hwnd, &mut wheel) }, 0);
            window
                .post_input(WM_MOUSEWHEEL, 120 << 16, position(wheel.x, wheel.y))
                .unwrap();
            assert!(window.pump().into_iter().any(|event| matches!(event, Event::Input { message: WM_MOUSEWHEEL, lparam, .. } if lparam == position(400, 300))));
            frame.width = 320;
            frame.height = 240;
            frame.bgra.resize(320 * 240 * 4, 255);
            window.present(&frame).unwrap();
            assert_eq!(
                client(&window),
                (320, 240),
                "a real later mode change must restore 1:1"
            );
        }

        #[test]
        fn gdi_native_pixels_and_integer_upscale_remain_exact_with_black_bars() {
            initialize_dpi().unwrap();
            let mut frame = Snapshot {
                width: 8,
                height: 6,
                bgra: Vec::new(),
                frames: 1,
            };
            for y in 0..6u8 {
                for x in 0..8u8 {
                    frame.bgra.extend([x * 31, y * 37, (x ^ y) * 17, 255]);
                }
            }
            for (width, height, scale, top) in [(8, 6, 1, 0), (16, 16, 2, 2)] {
                // A real 32-bit DIB/DC exercises the same GDI presenter as HWND output.
                unsafe {
                    let dc = CreateCompatibleDC(null_mut());
                    assert!(!dc.is_null());
                    let info = BITMAPINFO {
                        bmiHeader: BITMAPINFOHEADER {
                            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                            biWidth: width,
                            biHeight: -height,
                            biPlanes: 1,
                            biBitCount: 32,
                            biCompression: BI_RGB,
                            ..std::mem::zeroed()
                        },
                        ..std::mem::zeroed()
                    };
                    let mut bits = null_mut();
                    let bitmap =
                        CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
                    assert!(!bitmap.is_null());
                    let old = SelectObject(dc, bitmap);
                    paint_frame(
                        dc,
                        &RECT {
                            left: 0,
                            top: 0,
                            right: width,
                            bottom: height,
                        },
                        &frame,
                    )
                    .unwrap();
                    for y in 0..height {
                        for x in 0..width {
                            let expected = if y < top || y >= top + 6 * scale {
                                0
                            } else {
                                let at = (((y - top) / scale * 8 + x / scale) * 4) as usize;
                                u32::from(frame.bgra[at + 2])
                                    | (u32::from(frame.bgra[at + 1]) << 8)
                                    | (u32::from(frame.bgra[at]) << 16)
                            };
                            assert_eq!(
                                GetPixel(dc, x, y),
                                expected,
                                "host pixel ({x},{y}) at {width}x{height}"
                            );
                        }
                    }
                    SelectObject(dc, old);
                    DeleteObject(bitmap);
                    DeleteDC(dc);
                }
            }
        }
    }
}

#[cfg(windows)]
pub use native::{Window, initialize_dpi};

#[cfg(not(windows))]
pub fn initialize_dpi() -> Result<(), String> {
    Ok(())
}

#[cfg(not(windows))]
pub struct Window;
#[cfg(not(windows))]
impl Window {
    pub fn next_refresh(&self) -> std::time::Instant {
        std::time::Instant::now() + Duration::from_nanos(16_666_667)
    }
    pub fn new(_: &str, _: u32, _: u32, _: bool) -> Result<Self, String> {
        Err("native window requires Windows; use --headless".into())
    }
    pub fn pump(&mut self) -> Vec<Event> {
        Vec::new()
    }
    pub fn post_input(&mut self, _: u32, _: u32, _: u32) -> Result<(), String> {
        Ok(())
    }
    pub fn present(&mut self, _: &Snapshot) -> Result<(), String> {
        Ok(())
    }
    pub fn wait(&mut self, _: Option<Duration>) -> Result<WaitOutcome, String> {
        Ok(WaitOutcome::Deadline)
    }
    pub fn wait_with_wake(
        &mut self,
        _: Option<Duration>,
        _: Option<&WakeEvent>,
    ) -> Result<WaitOutcome, String> {
        Ok(WaitOutcome::Deadline)
    }
    pub fn hide(&mut self) {}
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn native_focus_loss_reaches_the_frontend_event_pump() {
        let mut window = Window::new("focus regression", 320, 240, true).unwrap();
        window
            .post_input(
                windows_sys::Win32::UI::WindowsAndMessaging::WM_KILLFOCUS,
                0,
                0,
            )
            .unwrap();
        assert!(
            window
                .pump()
                .iter()
                .any(|event| matches!(event, Event::FocusLost))
        );
    }

    #[test]
    fn high_resolution_deadline_and_host_input_both_wake_the_window() {
        let mut window = Window::new("timer regression", 320, 240, true).unwrap();
        window.pump();
        assert_eq!(
            window.wait(Some(Duration::from_millis(2))).unwrap(),
            WaitOutcome::Deadline
        );
        window
            .post_input(
                windows_sys::Win32::UI::WindowsAndMessaging::WM_KEYDOWN,
                0x41,
                1,
            )
            .unwrap();
        assert_eq!(
            window.wait(Some(Duration::from_secs(1))).unwrap(),
            WaitOutcome::Input
        );
        assert!(
            window
                .pump()
                .iter()
                .any(|event| matches!(event, Event::Input { message: 0x100, .. }))
        );
        assert_eq!(
            window.wait(Some(Duration::from_millis(2))).unwrap(),
            WaitOutcome::Deadline
        );
    }

    #[test]
    fn owned_control_event_wakes_an_indefinite_window_wait() {
        let mut window = Window::new("control wake regression", 320, 240, true).unwrap();
        window.pump();
        let wake = WakeEvent::new().unwrap();
        wake.signal();
        assert_eq!(
            window.wait_with_wake(None, Some(&wake)).unwrap(),
            WaitOutcome::Input
        );
        assert_eq!(
            window
                .wait_with_wake(Some(Duration::from_millis(2)), Some(&wake))
                .unwrap(),
            WaitOutcome::Deadline
        );
    }
}
