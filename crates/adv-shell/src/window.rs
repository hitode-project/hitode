#[cfg(not(target_os = "macos"))]
use adv_runtime::Snapshot;
pub use adv_sync::WakeEvent;
#[cfg(target_os = "macos")]
pub(crate) use macos::native_viewport;
#[cfg(all(windows, feature = "gpu"))]
pub(crate) use native::native_viewport;
use std::sync::Arc;
use std::time::Duration;

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub enum Event {
    Menu(adv_runtime::MenuEvent),
    Dialog(adv_runtime::DialogEvent),
    Input {
        message: u32,
        wparam: u32,
        lparam: u32,
    },
    #[cfg(any(target_os = "macos", target_os = "android"))]
    Text(String, std::sync::mpsc::SyncSender<usize>),
    Close,
    #[cfg(any(target_os = "macos", target_os = "android"))]
    ForceClose,
    FocusLost,
    #[cfg(any(target_os = "macos", target_os = "android"))]
    AudioSuspended(bool),
    /// The launcher paused the game: no guest execution, frozen guest time
    /// and suspended audio until it resumes.
    #[cfg(any(target_os = "macos", target_os = "android"))]
    Paused(bool),
    #[cfg(any(target_os = "macos", target_os = "android"))]
    Volume(u8),
    Repaint,
}
pub type MenuSink = Box<dyn FnMut(adv_runtime::MenuEvent) -> Result<(), String>>;
pub type DialogSink = Box<dyn FnMut(adv_runtime::DialogEvent) -> Result<(), String>>;

#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub enum WaitOutcome {
    Input,
    Deadline,
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::collections::VecDeque;
    use std::ptr::{null, null_mut};
    mod dialog;
    mod menu;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0,
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
        menu: Option<menu::HostMenu>,
        popup: Option<menu::HostMenu>,
        menu_sink: Option<MenuSink>,
        events: VecDeque<Event>,
        width: u32,
        height: u32,
        client_width: i32,
        client_height: i32,
        closed: bool,
        cursor_visible: bool,
        video_cursor_regions: Arc<[adv_runtime::VideoCursorRegion]>,
        retained: RetainedFrame,
        paint_error: Option<String>,
        #[cfg(feature = "gpu")]
        gpu_mode: bool,
    }
    impl State {
        fn cursor_visible_at(&self, point: &POINT, client: &RECT) -> bool {
            let (x, y) = Viewport::fit(client, self.width, self.height).position(
                point.x,
                point.y,
                self.width,
                self.height,
            );
            self.cursor_visible
                && !adv_runtime::VideoCursorRegion::hidden_at(&self.video_cursor_regions, x, y)
        }
    }
    pub struct Window {
        hwnd: HWND,
        state: Box<State>,
        timer: HANDLE,
        frame_size: Option<(u32, u32)>,
        dialogs: dialog::HostDialogs,
        hidden: bool,
    }

    pub fn initialize_dpi() -> Result<(), String> {
        static RESULT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
        RESULT
            .get_or_init(|| {
                // Set the process default before font/DC creation and the first
                // HWND. A manifest or compatibility setting may have fixed it.
                unsafe {
                    if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
                        != 0
                    {
                        Ok(())
                    } else {
                        let error = GetLastError();
                        if error == windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED {
                            Ok(()) // An existing process default needs a thread override.
                        } else {
                            Err(format!("SetProcessDpiAwarenessContext failed: {error}"))
                        }
                    }
                }
            })
            .clone()?;
        // Thread overrides are not inherited by new threads. Check every caller,
        // including the execution thread that creates runtime fonts and DCs.
        unsafe {
            if AreDpiAwarenessContextsEqual(
                GetThreadDpiAwarenessContext(),
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            ) == 0
                && SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
                    .is_null()
            {
                return Err(format!(
                    "SetThreadDpiAwarenessContext failed: {}",
                    GetLastError()
                ));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod dpi_tests {
        use super::*;

        #[test]
        fn dpi_initialization_handles_fixed_process_default_and_each_thread() {
            const CHILD: &str = "ADV_SHELL_DPI_TEST_CHILD";
            if std::env::var_os(CHILD).is_none() {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "window::native::dpi_tests::dpi_initialization_handles_fixed_process_default_and_each_thread",
                        "--nocapture",
                    ])
                    .env(CHILD, "1")
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
            // Use a separate process so no other tests inherit this fixed default.
            unsafe {
                if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE) == 0 {
                    assert_eq!(
                        GetLastError(),
                        windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED
                    );
                }
                assert!(!SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE).is_null());
                assert_eq!(
                    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2),
                    0
                );
                assert_eq!(
                    GetLastError(),
                    windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED
                );
            }
            initialize_dpi().unwrap();
            for _ in 0..2 {
                std::thread::spawn(|| unsafe {
                    assert!(
                        !SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE).is_null()
                    );
                    for _ in 0..2 {
                        initialize_dpi().unwrap();
                        assert_ne!(
                            AreDpiAwarenessContextsEqual(
                                GetThreadDpiAwarenessContext(),
                                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                            ),
                            0
                        );
                    }
                })
                .join()
                .unwrap();
            }
        }
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

    #[cfg(feature = "gpu")]
    pub(crate) fn native_viewport(
        width: u32,
        height: u32,
        canvas_width: u32,
        canvas_height: u32,
    ) -> [f32; 4] {
        let viewport = Viewport::fit(
            &RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            },
            canvas_width,
            canvas_height,
        );
        [
            viewport.x as f32,
            viewport.y as f32,
            viewport.width as f32,
            viewport.height as f32,
        ]
    }

    fn outer_rect(width: i32, height: i32, dpi: u32, menu: bool) -> Result<RECT, String> {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        // SAFETY: valid rectangle and the owned window's actual menu state.
        if unsafe {
            AdjustWindowRectExForDpi(&mut rect, WS_OVERLAPPEDWINDOW, i32::from(menu), 0, dpi)
        } == 0
        {
            return Err(format!("AdjustWindowRectExForDpi failed: {}", unsafe {
                GetLastError()
            }));
        }
        Ok(rect)
    }

    fn set_client_size(hwnd: HWND, width: u32, height: u32) -> Result<(), String> {
        let rect = outer_rect(
            width as i32,
            height as i32,
            unsafe { GetDpiForWindow(hwnd) },
            !unsafe { GetMenu(hwnd) }.is_null(),
        )?;
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

    /// UI-owned storage. GDI handles and the DIB pointer never leave this thread.
    struct Dib {
        dc: HDC,
        bitmap: HBITMAP,
        original: HGDIOBJ,
        bits: *mut u8,
        width: i32,
        height: i32,
    }
    impl Dib {
        fn new(width: i32, height: i32) -> Result<Self, String> {
            if width <= 0 || height <= 0 {
                return Err("invalid presentation buffer dimensions".into());
            }
            // SAFETY: this memory DC and selected top-down DIB are owned together.
            unsafe {
                let dc = CreateCompatibleDC(null_mut());
                if dc.is_null() {
                    return Err("CreateCompatibleDC failed".into());
                }
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
                let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
                if bitmap.is_null() || bits.is_null() {
                    if !bitmap.is_null() {
                        DeleteObject(bitmap);
                    }
                    DeleteDC(dc);
                    return Err("CreateDIBSection failed".into());
                }
                let original = SelectObject(dc, bitmap);
                if original.is_null() || original as isize == -1 {
                    DeleteObject(bitmap);
                    DeleteDC(dc);
                    return Err("SelectObject failed for presentation buffer".into());
                }
                Ok(Self {
                    dc,
                    bitmap,
                    original,
                    bits: bits.cast(),
                    width,
                    height,
                })
            }
        }
    }
    impl Drop for Dib {
        fn drop(&mut self) {
            // SAFETY: finish queued reads before releasing storage and restore
            // the stock bitmap before deleting the owned bitmap/DC.
            unsafe {
                GdiFlush();
                SelectObject(self.dc, self.original);
                DeleteObject(self.bitmap);
                DeleteDC(self.dc);
            }
        }
    }

    #[derive(Default)]
    struct RetainedFrame {
        source: Option<Dib>,
        composition: Option<Dib>,
    }
    impl RetainedFrame {
        fn update(&mut self, frame: &Snapshot) -> Result<(), String> {
            let length = (frame.width as usize)
                .checked_mul(frame.height as usize)
                .and_then(|n| n.checked_mul(4));
            if frame.width == 0
                || frame.height == 0
                || frame.width > i32::MAX as u32
                || frame.height > i32::MAX as u32
                || length != Some(frame.bgra.len())
            {
                return Err("invalid guest frame dimensions or pixels".into());
            }
            let (width, height) = (frame.width as i32, frame.height as i32);
            // A prior BitBlt may still read the DIB. Flush before replacing or
            // writing it; no window pixels are touched during this upload.
            if unsafe { GdiFlush() } == 0 {
                return Err("GdiFlush failed before frame upload".into());
            }
            if self
                .source
                .as_ref()
                .is_none_or(|s| (s.width, s.height) != (width, height))
            {
                self.source = Some(Dib::new(width, height)?);
            }
            let source = self.source.as_ref().unwrap();
            // SAFETY: checked packed BGRA length matches the owned DIB. No GDI
            // reads remain in flight, and callbacks run on this same UI thread.
            unsafe {
                std::ptr::copy_nonoverlapping(frame.bgra.as_ptr(), source.bits, frame.bgra.len())
            };
            Ok(())
        }

        fn prepare(&mut self, client: &RECT) -> Result<Option<&Dib>, String> {
            let (width, height) = (client.right - client.left, client.bottom - client.top);
            if width <= 0 || height <= 0 {
                return Ok(None);
            }
            let Some(source) = &self.source else {
                return Ok(None);
            };
            // Native-size output only needs the final blit; no clear or scale.
            if (source.width, source.height) == (width, height) {
                return Ok(Some(source));
            }
            if self
                .composition
                .as_ref()
                .is_none_or(|s| (s.width, s.height) != (width, height))
            {
                self.composition = Some(Dib::new(width, height)?);
            }
            let canvas = self.composition.as_ref().unwrap();
            let viewport = Viewport::fit(client, source.width as u32, source.height as u32);
            // All clear/scale operations are PRIVATE. A GDI batch flush between
            // them cannot expose a black intermediate frame to the compositor.
            unsafe {
                if PatBlt(canvas.dc, 0, 0, width, height, BLACKNESS) == 0
                    || SetStretchBltMode(canvas.dc, COLORONCOLOR) == 0
                    || StretchBlt(
                        canvas.dc,
                        viewport.x,
                        viewport.y,
                        viewport.width,
                        viewport.height,
                        source.dc,
                        0,
                        0,
                        source.width,
                        source.height,
                        SRCCOPY,
                    ) == 0
                {
                    return Err("offscreen frame composition failed".into());
                }
            }
            Ok(Some(canvas))
        }

        fn draw(&mut self, dc: HDC, client: &RECT) -> Result<(), String> {
            let Some(complete) = self.prepare(client)? else {
                // Only before the first guest frame, or for an empty client.
                // Once a frame exists, idle paints always use retained pixels.
                if self.source.is_none()
                    && client.right > 0
                    && client.bottom > 0
                    && unsafe { PatBlt(dc, 0, 0, client.right, client.bottom, BLACKNESS) } == 0
                {
                    return Err("initial background paint failed".into());
                }
                return Ok(());
            };
            // The ONLY write to a populated window is a complete opaque frame.
            if unsafe {
                BitBlt(
                    dc,
                    0,
                    0,
                    complete.width,
                    complete.height,
                    complete.dc,
                    0,
                    0,
                    SRCCOPY,
                )
            } == 0
            {
                return Err("BitBlt frame commit failed".into());
            }
            if unsafe { GdiFlush() } == 0 {
                return Err("GdiFlush failed after frame commit".into());
            }
            Ok(())
        }
    }

    #[cfg(test)]
    fn paint_frame(dc: HDC, client: &RECT, frame: &Snapshot) -> Result<(), String> {
        let mut retained = RetainedFrame::default();
        retained.update(frame)?;
        retained.draw(dc, client)
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
                // BeginPaint may synchronously send WM_ERASEBKGND. Do not hold
                // a mutable State borrow across it or recreate one for erase.
                if message == WM_ERASEBKGND {
                    return 1;
                }
                if message == WM_PAINT {
                    let mut paint = std::mem::zeroed();
                    let dc = BeginPaint(hwnd, &mut paint);
                    let mut rect = std::mem::zeroed();
                    #[cfg(feature = "gpu")]
                    if (*pointer).gpu_mode {
                        // DWM retains the previous opaque swapchain image. UI
                        // records a new draw at its next refresh opportunity.
                        EndPaint(hwnd, &paint);
                        (*pointer).events.push_back(Event::Repaint);
                        return 0;
                    }
                    let result = if dc.is_null() || GetClientRect(hwnd, &mut rect) == 0 {
                        Err("BeginPaint/GetClientRect failed".into())
                    } else {
                        // Restore while the paint DC is valid, even when the
                        // guest is idle or no newer frame has reached UI.
                        (*pointer).retained.draw(dc, &rect)
                    };
                    EndPaint(hwnd, &paint);
                    if let Err(error) = result {
                        (*pointer).paint_error = Some(error);
                    }
                    return 0;
                }
                if message == WM_DPICHANGED {
                    let suggested = *(lparam as *const RECT);
                    // Preserve the chosen physical client size across monitors;
                    // only the native border/caption should follow the new DPI.
                    if let Ok(rect) = outer_rect(
                        (*pointer).client_width,
                        (*pointer).client_height,
                        (wparam as u32 & 0xffff).max(96),
                        !GetMenu(hwnd).is_null(),
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
                if message == WM_TIMER && wparam == menu::POPUP_TIMER {
                    if state.popup.as_ref().is_some_and(menu::HostMenu::ended) {
                        EndMenu();
                    }
                    return 0;
                }
                if let Some(event) = state
                    .popup
                    .as_ref()
                    .and_then(|menu| menu.event(message, wparam, lparam))
                    .or_else(|| {
                        state
                            .menu
                            .as_ref()
                            .and_then(|menu| menu.event(message, wparam, lparam))
                    })
                {
                    if let Some(sink) = state.menu_sink.as_mut() {
                        if let Err(error) = sink(event) {
                            state.paint_error = Some(error);
                        }
                    } else {
                        state.events.push_back(Event::Menu(event));
                    }
                    return 0;
                }
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
                        // The owned dialog projects the guest's own focus transfer.
                        // Moving into it is not an application focus loss.
                        if wparam != 0 && GetAncestor(wparam as HWND, GA_ROOTOWNER) == hwnd {
                            return 0;
                        }
                        state.events.push_back(Event::FocusLost);
                        return 0;
                    }
                    WM_CLOSE => {
                        if state.popup.is_some() {
                            EndMenu();
                        }
                        if !state.closed {
                            state.events.push_back(Event::Close);
                            state.closed = true;
                        }
                        ShowWindow(hwnd, SW_HIDE);
                        return 0;
                    }
                    WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
                        if state.menu.is_some() && matches!(message, WM_SYSKEYDOWN | WM_SYSKEYUP) {
                            return DefWindowProcW(hwnd, message, wparam, lparam);
                        }
                        state.events.push_back(Event::Input {
                            message,
                            wparam: wparam as u32,
                            lparam: lparam as u32,
                        });
                        return 0;
                    }
                    // The ANSI guest's TranslateMessage generates its characters.
                    // Host UTF-16 character messages must not enter that queue.
                    WM_SYSCHAR if state.menu.is_some() => {
                        return DefWindowProcW(hwnd, message, wparam, lparam);
                    }
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
                        // Windows messages are already logical. Normalize to
                        // physical buttons before publishing live key state.
                        let (physical_message, physical_buttons) = adv_runtime::mouse_mapping(
                            message,
                            wparam as u32,
                            GetSystemMetrics(SM_SWAPBUTTON) != 0,
                        );
                        state.events.push_back(Event::Input {
                            message: physical_message,
                            wparam: physical_buttons,
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
                    WM_SETCURSOR
                        if wparam as HWND == hwnd && lparam as u32 & 0xffff == HTCLIENT =>
                    {
                        let mut point = std::mem::zeroed();
                        let mut client: RECT = std::mem::zeroed();
                        let visible = if GetCursorPos(&mut point) != 0
                            && ScreenToClient(hwnd, &mut point) != 0
                            && GetClientRect(hwnd, &mut client) != 0
                        {
                            state.cursor_visible_at(&point, &client)
                        } else {
                            state.cursor_visible
                        };
                        SetCursor(if visible {
                            LoadCursorW(null_mut(), IDC_ARROW)
                        } else {
                            null_mut()
                        });
                        return 1;
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
                menu: None,
                popup: None,
                menu_sink: None,
                events: VecDeque::new(),
                width,
                height,
                client_width: width as i32,
                client_height: height as i32,
                closed: false,
                cursor_visible: true,
                video_cursor_regions: Arc::from([]),
                retained: RetainedFrame::default(),
                paint_error: None,
                #[cfg(feature = "gpu")]
                gpu_mode: false,
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
                let rect = outer_rect(width as i32, height as i32, GetDpiForSystem(), false)?;
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
                dialogs: dialog::HostDialogs::default(),
                hidden,
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
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
            }
            self.state
                .events
                .extend(self.dialogs.take_events().map(Event::Dialog));
            self.state.events.drain(..).collect()
        }

        /// Change the canvas cursor without modifying Win32's global ShowCursor
        /// counter. Non-client areas and other applications keep their cursors.
        pub fn set_cursor_visible(&mut self, visible: bool) {
            if self.state.cursor_visible == visible {
                return;
            }
            self.state.cursor_visible = visible;
            self.refresh_cursor();
        }

        pub fn set_video_cursor_regions(&mut self, regions: Arc<[adv_runtime::VideoCursorRegion]>) {
            self.state.video_cursor_regions = regions;
            self.refresh_cursor();
        }

        fn refresh_cursor(&mut self) {
            unsafe {
                let mut point = std::mem::zeroed();
                let mut client: RECT = std::mem::zeroed();
                if GetCursorPos(&mut point) != 0
                    && WindowFromPoint(point) == self.hwnd
                    && ScreenToClient(self.hwnd, &mut point) != 0
                    && GetClientRect(self.hwnd, &mut client) != 0
                    && point.x >= 0
                    && point.y >= 0
                    && point.x < client.right
                    && point.y < client.bottom
                {
                    SetCursor(if self.state.cursor_visible_at(&point, &client) {
                        LoadCursorW(null_mut(), IDC_ARROW)
                    } else {
                        null_mut()
                    });
                }
            }
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
            #[cfg(feature = "gpu")]
            if self.state.gpu_mode {
                return Err("GPU window frames must use the owned compositor".into());
            }
            self.state.retained.update(frame)?;
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
                if GetClientRect(self.hwnd, &mut rect) == 0 {
                    ReleaseDC(self.hwnd, dc);
                    return Err("GetClientRect failed".into());
                }
                // Aspect-preserving fit with explicit nearest pixel sampling.
                // Letterboxing never changes the guest canvas or input space.
                let result = self.state.retained.draw(dc, &rect);
                ReleaseDC(self.hwnd, dc);
                result?;
            }
            Ok(())
        }

        /// Startup preparation, before an execution worker can submit to this
        /// device. Normal paint/present never configures or waits for queue idle.
        #[cfg(feature = "gpu")]
        pub(crate) fn create_native(
            &mut self,
            fallback: bool,
        ) -> Result<(adv_device::gpu::Gpu, crate::native_gpu::WindowPresenter), String> {
            let width = self.state.client_width.max(1) as u32;
            let height = self.state.client_height.max(1) as u32;
            // SAFETY: the assembly joins backend tasks before destroying HWND.
            let (gpu, mut presenter) = pollster::block_on(unsafe {
                crate::native_gpu::WindowPresenter::request(self.hwnd, width, height, fallback)
            })?;
            presenter.configure_exclusive(width, height)?;
            self.state.gpu_mode = true;
            Ok((gpu, presenter))
        }

        #[cfg(all(test, feature = "gpu"))]
        pub(crate) fn prepare_native(
            &mut self,
            context: adv_device::gpu::NativeContext,
        ) -> Result<crate::native_gpu::WindowPresenter, String> {
            let width = self.state.client_width.max(1) as u32;
            let height = self.state.client_height.max(1) as u32;
            // SAFETY: the assembly joins required backend tasks and drops the
            // returned presenter before destroying this HWND.
            let mut presenter = unsafe {
                crate::native_gpu::WindowPresenter::new(self.hwnd, context, width, height)?
            };
            presenter.configure_exclusive(width, height)?;
            self.state.gpu_mode = true;
            Ok(presenter)
        }

        /// UI-only geometry and input mapping. No GPU call takes place here.
        #[cfg(feature = "gpu")]
        pub fn native_client_size(&self) -> Result<(u32, u32), String> {
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            if unsafe { GetClientRect(self.hwnd, &mut rect) } == 0 {
                return Err("GetClientRect for GPU window failed".into());
            }
            Ok((rect.right.max(0) as u32, rect.bottom.max(0) as u32))
        }

        #[cfg(feature = "gpu")]
        pub fn native_canvas(&mut self, canvas: (u32, u32)) -> Result<bool, String> {
            if canvas.0 == 0 || canvas.1 == 0 {
                return Ok(false);
            }
            self.state.width = canvas.0;
            self.state.height = canvas.1;
            let changed = self.frame_size != Some(canvas);
            if changed {
                set_client_size(self.hwnd, canvas.0, canvas.1)?;
                self.frame_size = Some(canvas);
            }
            Ok(changed)
        }

        #[allow(dead_code)] // Also used by the standalone wait_probe example.
        pub fn wait(&mut self, duration: Option<Duration>) -> Result<WaitOutcome, String> {
            self.wait_with_wake(duration, None)
        }

        pub fn hide(&mut self) {
            self.hidden = true;
            self.dialogs.hide();
            unsafe {
                ShowWindow(self.hwnd, SW_HIDE);
            }
        }

        pub fn wait_with_wake(
            &mut self,
            duration: Option<Duration>,
            wake: Option<&WakeEvent>,
        ) -> Result<WaitOutcome, String> {
            if let Some(error) = self.dialogs.take_error() {
                return Err(error);
            }
            if let Some(error) = self.state.paint_error.take() {
                return Err(error);
            }
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
            self.dialogs.clear();
            // SAFETY: state remains live until synchronous destruction and callbacks finish.
            unsafe {
                if self.state.menu.is_some() {
                    SetMenu(self.hwnd, null_mut());
                    drop(self.state.menu.take());
                }
                CancelWaitableTimer(self.timer);
                CloseHandle(self.timer);
                DestroyWindow(self.hwnd);
            }
        }
    }

    #[cfg(test)]
    mod desktop_tests {
        include!("window_desktop_tests.rs");
    }

    #[cfg(test)]
    mod presentation_tests {
        use super::*;

        // spec: HOST-VIEW-2
        #[test]
        fn composing_a_frame_keeps_the_previous_output_until_commit() {
            let target = Dib::new(16, 16).unwrap();
            let rect = RECT {
                left: 0,
                top: 0,
                right: 16,
                bottom: 16,
            };
            let mut frame = Snapshot {
                width: 8,
                height: 6,
                bgra: [32, 64, 192, 255].repeat(8 * 6),
                frames: 1,
                ..Default::default()
            };
            let mut retained = RetainedFrame::default();
            retained.update(&frame).unwrap();
            retained.draw(target.dc, &rect).unwrap();
            let previous = unsafe { GetPixel(target.dc, 8, 8) };
            assert_eq!(previous, 0x2040c0);

            // Upload and every offscreen GDI operation may be preempted/flushed.
            // None may expose the clear used for composing the next frame.
            frame.bgra = [224, 128, 16, 255].repeat(8 * 6);
            retained.update(&frame).unwrap();
            retained.prepare(&rect).unwrap();
            assert_ne!(unsafe { GdiFlush() }, 0);
            assert_eq!(unsafe { GetPixel(target.dc, 8, 8) }, previous);
            retained.draw(target.dc, &rect).unwrap();
            assert_eq!(unsafe { GetPixel(target.dc, 8, 8) }, 0xe08010);
            assert_eq!(unsafe { GetPixel(target.dc, 8, 0) }, 0);
        }

        // spec: HOST-VIEW-2
        #[test]
        fn retained_pixels_survive_caller_reuse_and_invalid_frames_but_allow_black() {
            let target = Dib::new(8, 6).unwrap();
            let rect = RECT {
                left: 0,
                top: 0,
                right: 8,
                bottom: 6,
            };
            let mut frame = Snapshot {
                width: 8,
                height: 6,
                bgra: [32, 64, 192, 255].repeat(8 * 6),
                frames: 1,
                ..Default::default()
            };
            let mut retained = RetainedFrame::default();
            retained.update(&frame).unwrap();
            // Mailbox storage is reusable immediately after upload.
            frame.bgra.fill(0);
            retained.draw(target.dc, &rect).unwrap();
            assert_eq!(unsafe { GetPixel(target.dc, 4, 3) }, 0x2040c0);
            let invalid = Snapshot {
                width: 0,
                height: 6,
                bgra: Vec::new(),
                frames: 2,
                ..Default::default()
            };
            assert!(retained.update(&invalid).is_err());
            let short = Snapshot {
                width: 8,
                height: 6,
                bgra: vec![0; 8],
                frames: 2,
                ..Default::default()
            };
            assert!(retained.update(&short).is_err());
            retained.draw(target.dc, &rect).unwrap();
            assert_eq!(unsafe { GetPixel(target.dc, 4, 3) }, 0x2040c0);

            // Real guest fades/black frames must remain visible, never filtered.
            retained.update(&frame).unwrap();
            retained.draw(target.dc, &rect).unwrap();
            assert_eq!(unsafe { GetPixel(target.dc, 4, 3) }, 0);
        }

        fn client(window: &Window) -> (i32, i32) {
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { GetClientRect(window.hwnd, &mut rect) }, 0);
            (rect.right, rect.bottom)
        }
        fn position(x: i32, y: i32) -> u32 {
            u32::from(x as u16) | (u32::from(y as u16) << 16)
        }

        // spec: HOST-VIEW-1
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
                ..Default::default()
            };
            window.present(&frame).unwrap();
            assert_eq!(
                client(&window),
                (800, 600),
                "guest mode change must use native physical pixels"
            );
            let dpi = unsafe { GetDpiForWindow(window.hwnd) };
            println!("PMv2 window DPI={dpi}; initial guest/client=800x600 physical pixels");
            let rect = outer_rect(800, 800, dpi, false).unwrap();
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

        // spec: HOST-VIEW-1
        #[test]
        fn gdi_native_pixels_and_integer_upscale_remain_exact_with_black_bars() {
            initialize_dpi().unwrap();
            let mut frame = Snapshot {
                width: 8,
                height: 6,
                bgra: Vec::new(),
                frames: 1,
                ..Default::default()
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

#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::Projection;
#[cfg(target_os = "macos")]
pub use macos::{GameContainer, GameView, Window, initialize_dpi};

#[cfg(not(any(windows, target_os = "macos")))]
pub fn initialize_dpi() -> Result<(), String> {
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub struct Window;
#[cfg(not(any(windows, target_os = "macos")))]
impl Window {
    pub fn set_video_cursor_regions(&mut self, _: Arc<[adv_runtime::VideoCursorRegion]>) {}
    pub fn set_cursor_visible(&mut self, _: bool) {}
    pub fn set_dialogs(&mut self, _: &adv_runtime::DialogSet) -> Result<(), String> {
        Ok(())
    }
    pub fn set_dialog_event_sink(&mut self, _: DialogSink) {}
    pub fn set_menu_bar(&mut self, _: &adv_runtime::MenuBar) -> Result<(), String> {
        Ok(())
    }
    pub fn set_menu_event_sink(&mut self, _: MenuSink) {}
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

#[cfg(target_os = "android")]
pub(crate) mod android;
#[cfg(target_os = "android")]
pub(crate) use android::native_viewport;
#[cfg(target_os = "android")]
pub use android::{GameView, TouchAction};
#[cfg(any(target_os = "android", test))]
pub(crate) mod android_epoch;
#[cfg(all(test, not(target_os = "android")))]
#[path = "window/android/input.rs"]
mod android_input_tests;

#[cfg(any(target_os = "macos", target_os = "android"))]
/// The guest is told the display runs at 60 Hz, and both `Present` and
/// software frames are published at guest frame boundaries, so presenting
/// faster only repeats conversion and composition. Faster displays keep 10%
/// slack so a 60 Hz guest whose frames arrive early is still shown at once.
pub(crate) fn refresh_interval(display_fps: isize) -> Duration {
    let fps = display_fps.clamp(24, 240) as f64;
    if fps > 60.0 {
        Duration::from_secs_f64(0.9 / 60.0)
    } else {
        Duration::from_secs_f64(1.0 / fps)
    }
}
