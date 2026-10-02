use adv_runtime::Snapshot;
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
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateEventW,
        CreateWaitableTimerExW, SetEvent, SetWaitableTimerEx, TIMER_ALL_ACCESS,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    struct State {
        events: VecDeque<Event>,
        width: u32,
        height: u32,
        closed: bool,
    }
    pub struct Window {
        hwnd: HWND,
        state: Box<State>,
        timer: HANDLE,
    }

    struct WakeHandle(HANDLE);
    // SAFETY: this owned kernel event is only signalled or waited on. Windows
    // supports both across threads; Arc keeps the handle live during every call.
    unsafe impl Send for WakeHandle {}
    unsafe impl Sync for WakeHandle {}
    impl Drop for WakeHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    #[derive(Clone)]
    pub struct WakeEvent(std::sync::Arc<WakeHandle>);
    impl WakeEvent {
        pub fn new() -> Result<Self, String> {
            // SAFETY: an unnamed auto-reset kernel event has no external pointers.
            let handle = unsafe { CreateEventW(null(), 0, 0, null()) };
            if handle.is_null() {
                return Err(format!("CreateEventW failed: {}", unsafe {
                    GetLastError()
                }));
            }
            Ok(Self(std::sync::Arc::new(WakeHandle(handle))))
        }
        pub fn signal(&self) {
            unsafe {
                SetEvent(self.0.0);
            }
        }
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
                let state = &mut *pointer;
                match message {
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
                        let x = point.x * state.width as i32 / (rect.right - rect.left).max(1);
                        let y = point.y * state.height as i32 / (rect.bottom - rect.top).max(1);
                        let position = u32::from(x as u16) | (u32::from(y as u16) << 16);
                        state.events.push_back(Event::Input {
                            message,
                            wparam: wparam as u32,
                            lparam: position,
                        });
                        if message == WM_LBUTTONDOWN || message == WM_RBUTTONDOWN {
                            SetCapture(hwnd);
                        }
                        if message == WM_LBUTTONUP || message == WM_RBUTTONUP {
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
        pub fn new(title: &str, width: u32, height: u32, hidden: bool) -> Result<Self, String> {
            let class: Vec<u16> = "AdvGuestHost\0".encode_utf16().collect();
            let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
            let mut state = Box::new(State {
                events: VecDeque::new(),
                width,
                height,
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
                let mut rect = RECT {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                };
                AdjustWindowRect(&mut rect, style, 0);
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
            Ok(Self { hwnd, state, timer })
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
            self.state.width = frame.width;
            self.state.height = frame.height;
            // SAFETY: the pixel array has packed BGRA rows and is valid throughout the call.
            unsafe {
                let dc = GetDC(self.hwnd);
                if dc.is_null() {
                    return Err("GetDC failed".into());
                }
                let mut rect: RECT = std::mem::zeroed();
                GetClientRect(self.hwnd, &mut rect);
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
                    0,
                    0,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    0,
                    0,
                    frame.width as i32,
                    frame.height as i32,
                    frame.bgra.as_ptr().cast(),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                ReleaseDC(self.hwnd, dc);
                if result == 0 || result == -1 {
                    return Err("StretchDIBits failed".into());
                }
            }
            Ok(())
        }

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
                handles[count] = wake.0.0;
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
}

#[cfg(windows)]
pub use native::{WakeEvent, Window};

#[cfg(not(windows))]
#[derive(Clone)]
pub struct WakeEvent;
#[cfg(not(windows))]
impl WakeEvent {
    pub fn new() -> Result<Self, String> {
        Ok(Self)
    }
    pub fn signal(&self) {}
}

#[cfg(not(windows))]
pub struct Window;
#[cfg(not(windows))]
impl Window {
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
