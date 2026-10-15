//! Native game input and Metal presentation surface.
use super::*;
use crate::window::refresh_interval;
use objc2_app_kit::NSView;
use objc2_quartz_core::{CADisplayLink, CAFrameRateRange, CAMetalLayer};

pub struct Input {
    hub: Rc<Hub>,
    width: u32,
    height: u32,
    cursor_visible: bool,
    cursor_hidden: bool,
    regions: Arc<[adv_runtime::VideoCursorRegion]>,
    wheel: f64,
    modifiers: usize,
    toggle_control: bool,
    control_latched: bool,
}

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = RefCell<Input>]
    pub struct GameView;
    unsafe impl NSObjectProtocol for GameView {}
    impl GameView {
        #[unsafe(method(viewDidMoveToWindow))]
        fn moved_to_window(&self) {
            let hub = self.hub();
            hub.main.set(self.window().map_or(0, |window| Retained::as_ptr(&window) as usize));
            hub.push(Event::Repaint);
        }
        #[unsafe(method(setFrameSize:))]
        fn resize(&self, size: NSSize) {
            unsafe { let _: () = msg_send![super(self), setFrameSize: size]; }
            self.hub().push(Event::Repaint);
        }
        #[unsafe(method(viewDidChangeBackingProperties))]
        fn backing_changed(&self) { self.hub().push(Event::Repaint); }
        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let mut input = self.ivars().borrow_mut();
            input.control_latched = false;
            input.hub.push(Event::FocusLost);
            if input.cursor_hidden { NSCursor::unhide(); input.cursor_hidden = false; }
            true
        }
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { true }
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) { self.ivars().borrow_mut().handle(self, event); }
        #[unsafe(method(frameClockFired:))]
        fn frame_clock_fired(&self, link: &CADisplayLink) { self.frame(link); }
    }
);

impl GameView {
    pub(super) fn viewport(&self, canvas: (u32, u32), scaling: crate::Scaling) -> Viewport {
        let scale = self
            .window()
            .map_or(1.0, |window| window.backingScaleFactor());
        Viewport::scaled(self.client(), canvas, scaling, scale)
    }
    pub(crate) fn clear_control_latch(&self) {
        let mut input = self.ivars().borrow_mut();
        if input.control_latched {
            input.control_latched = false;
            let (message, wparam, lparam) =
                keys::message(keys::key(0x3b).unwrap(), false, false, false);
            input.hub.push(Event::Input {
                message,
                wparam,
                lparam,
            });
        }
    }
    pub(crate) fn set_fast_forward_toggle(&self, toggle: bool) {
        let mut input = self.ivars().borrow_mut();
        if input.toggle_control != toggle {
            input.toggle_control = toggle;
            input.control_latched = false;
            input.hub.push(Event::FocusLost);
        }
    }

    /// Mouse lParams are client points, mapped like physical input.
    pub(crate) fn post_input(&self, message: u32, wparam: u32, lparam: u32) -> Result<(), String> {
        let lparam = if (0x200..=0x20a).contains(&message) {
            let (x, y) = (
                f64::from(lparam as u16 as i16),
                f64::from((lparam >> 16) as u16 as i16),
            );
            let viewport = self.viewport(self.hub().canvas.get(), self.hub().scaling.get());
            let (x, y) =
                viewport.position(x, y, self.hub().canvas.get().0, self.hub().canvas.get().1);
            u32::from(x as u16) | (u32::from(y as u16) << 16)
        } else {
            lparam
        };
        self.hub().push(Event::Input {
            message,
            wparam,
            lparam,
        });
        Ok(())
    }

    /// Create the Metal surface on the UI thread before guest submissions begin.
    pub(crate) fn create_native(
        &self,
        fallback: bool,
    ) -> Result<(adv_device::gpu::Gpu, crate::native_gpu::WindowPresenter), String> {
        let (width, height) = self.native_client_size()?;
        // SAFETY: the owner retains NSView until the background service joins
        // and the presenter drops. Surface creation stays on the main thread.
        let (gpu, mut presenter) = pollster::block_on(unsafe {
            crate::native_gpu::WindowPresenter::request(
                self as *const Self as *mut std::ffi::c_void,
                width.max(1),
                height.max(1),
                fallback,
            )
        })?;
        presenter.configure_exclusive(width.max(1), height.max(1))?;
        Ok((gpu, presenter))
    }

    pub(crate) fn native_client_size(&self) -> Result<(u32, u32), String> {
        if self.hub().occluded.get() || self.hub().sleeping.get() {
            return Ok((0, 0));
        }
        let (width, height) = self.client();
        let scale = self
            .window()
            .map_or(1.0, |window| window.backingScaleFactor());
        Ok((
            (width * scale).round() as u32,
            (height * scale).round() as u32,
        ))
    }

    pub(crate) fn native_canvas(&self, canvas: (u32, u32)) -> Result<bool, String> {
        if canvas.0 == 0 || canvas.1 == 0 {
            return Ok(false);
        }
        let changed = self.hub().canvas.get() != canvas;
        self.canvas(canvas.0, canvas.1);
        Ok(changed)
    }

    pub(crate) fn presentation(
        &self,
        menu: &adv_runtime::MenuBar,
        dialogs: &adv_runtime::DialogSet,
    ) -> Result<(), String> {
        let hub = self.hub();
        let enabled = dialogs
            .dialogs
            .iter()
            .filter(|dialog| {
                dialog.owner != 0
                    && !dialogs
                        .dialogs
                        .iter()
                        .any(|owner| owner.target.hwnd == dialog.owner)
            })
            .map(|dialog| dialog.owner_enabled)
            .reduce(|left, right| left && right);
        hub.main_enabled
            .set(enabled.unwrap_or(menu.key.window == 0 || menu.key.enabled));
        if let Some(error) = hub.dialogs.take_error() {
            return Err(error);
        }
        Ok(())
    }
    pub(crate) fn canvas_size(&self) -> (u32, u32) {
        self.hub().canvas.get()
    }
    pub(crate) fn watch(&self, wake: WakeEvent) {
        *self.hub().wake.borrow_mut() = Some(wake);
    }
    pub(crate) fn take_events(&self) -> Vec<Event> {
        self.hub().events.borrow_mut().drain(..).collect()
    }
    pub(crate) fn hide(&self) {
        self.setHidden(true);
        self.hub().occluded.set(true);
    }
    /// With a display link, presentation happens only inside its callback.
    pub(crate) fn refresh_ready(&self) -> bool {
        let hub = self.hub();
        hub.link.borrow().is_none() || hub.in_frame.get()
    }
    /// A display link caps the rate itself (`request_frame`); the timer
    /// fallback spaces presentations by the refresh interval.
    pub(crate) fn next_refresh(&self, paced: bool) -> std::time::Instant {
        let now = std::time::Instant::now();
        if self.hub().link.borrow().is_some() {
            return now;
        }
        now + refresh_interval(self.display_fps(), paced)
    }
    fn display_fps(&self) -> isize {
        self.window()
            .and_then(|window| window.screen())
            .map_or(60, |screen| screen.maximumFramesPerSecond())
    }
    /// Starts the display-link frame clock where AppKit provides one (macOS
    /// 14). It stays paused until `request_frame`; each callback publishes the
    /// vsync grid for guest pacing and runs `on_frame` once.
    pub(crate) fn start_frame_clock(&self, on_frame: Rc<dyn Fn()>) {
        let hub = self.hub();
        let available: bool =
            unsafe { msg_send![self, respondsToSelector: sel!(displayLinkWithTarget:selector:)] };
        if hub.link.borrow().is_some() || !available {
            return;
        }
        let link = unsafe { self.displayLinkWithTarget_selector(self, sel!(frameClockFired:)) };
        link.setPaused(true);
        unsafe {
            link.addToRunLoop_forMode(
                &objc2_foundation::NSRunLoop::mainRunLoop(),
                objc2_foundation::NSRunLoopCommonModes,
            );
        }
        *hub.on_frame.borrow_mut() = Some(on_frame);
        *hub.link.borrow_mut() = Some(link);
    }
    /// Invalidating the link releases its strong reference to this view.
    pub(crate) fn stop_frame_clock(&self) {
        let hub = self.hub();
        if let Some(link) = hub.link.borrow_mut().take() {
            link.invalidate();
        }
        hub.on_frame.borrow_mut().take();
        hub.link_paced.set(None);
        hub.vsync.clear();
    }
    /// Arms one display-link callback; false when the timer fallback is in use.
    pub(crate) fn request_frame(&self, paced: bool) -> bool {
        let hub = self.hub();
        let Some(link) = hub.link.borrow().clone() else {
            return false;
        };
        if hub.link_paced.replace(Some(paced)) != Some(paced) {
            let maximum = if paced {
                60.0
            } else {
                self.display_fps().max(60) as f32
            };
            link.setPreferredFrameRateRange(CAFrameRateRange {
                minimum: 30.0,
                maximum,
                preferred: maximum,
            });
        }
        link.setPaused(false);
        true
    }
    fn frame(&self, link: &CADisplayLink) {
        let hub = self.hub();
        link.setPaused(true);
        let period = link.duration();
        if period > 0.0 {
            let now = std::time::Instant::now();
            let ago = (objc2_quartz_core::CACurrentMediaTime() - link.timestamp()).max(0.0);
            let at = now.checked_sub(Duration::from_secs_f64(ago)).unwrap_or(now);
            hub.vsync.publish(at, Duration::from_secs_f64(period));
        }
        let on_frame = hub.on_frame.borrow().clone();
        if let Some(on_frame) = on_frame {
            hub.in_frame.set(true);
            on_frame();
            hub.in_frame.set(false);
        }
    }
    pub(crate) fn vsync(&self) -> Arc<adv_sync::vsync::Vsync> {
        self.hub().vsync.clone()
    }
    pub fn new(width: u32, height: u32) -> Result<Retained<Self>, String> {
        let mtm = MainThreadMarker::new().ok_or("the game view must run on the main thread")?;
        let hub = Rc::new(Hub::default());
        hub.canvas.set((width, height));
        hub.main_enabled.set(true);
        let this = Self::alloc(mtm).set_ivars(RefCell::new(Input {
            hub,
            width,
            height,
            cursor_visible: true,
            cursor_hidden: false,
            regions: Arc::from([]),
            wheel: 0.0,
            modifiers: 0,
            toggle_control: false,
            control_latched: false,
        }));
        let view: Retained<Self> = unsafe {
            msg_send![super(this), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(f64::from(width.max(1)), f64::from(height.max(1))))]
        };
        view.setWantsLayer(true);
        let layer = CAMetalLayer::new();
        // An integer-scaled frame keeps the game resolution and is magnified
        // here (native_gpu `Session::drawable`); other drawables match the client.
        unsafe {
            layer.setContentsGravity(objc2_quartz_core::kCAGravityResizeAspect);
            layer.setMagnificationFilter(objc2_quartz_core::kCAFilterNearest);
        }
        view.setLayer(Some(&layer));
        Ok(view)
    }
    pub(crate) fn hub(&self) -> Rc<Hub> {
        self.ivars().borrow().hub.clone()
    }
    pub(super) fn client(&self) -> (f64, f64) {
        let size = self.bounds().size;
        (size.width, size.height)
    }
    pub(super) fn canvas(&self, width: u32, height: u32) {
        let mut input = self.ivars().borrow_mut();
        input.width = width;
        input.height = height;
        input.hub.canvas.set((width, height));
        drop(input);
        if let Some(parent) = unsafe { self.superview() }
            && let Some(container) = parent.downcast_ref::<GameContainer>()
        {
            container.fit_canvas((width, height));
        }
    }
    pub(super) fn set_cursor_visible(&self, visible: bool) {
        let mut input = self.ivars().borrow_mut();
        if input.cursor_visible != visible {
            input.cursor_visible = visible;
            input.update_cursor(self, None);
        }
    }
    pub(crate) fn set_video_cursor_regions(&self, regions: Arc<[adv_runtime::VideoCursorRegion]>) {
        let mut input = self.ivars().borrow_mut();
        input.regions = regions;
        input.update_cursor(self, None);
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        if self.cursor_hidden {
            NSCursor::unhide();
        }
    }
}
impl Input {
    /// Translates input for the canvas; returns whether AppKit still gets it.
    fn handle(&mut self, view: &GameView, event: &NSEvent) -> bool {
        let main = self.hub.main.get();
        let in_main = event
            .window(view.mtm())
            .is_some_and(|window| Retained::as_ptr(&window) as usize == main);
        let kind = event.r#type();
        match kind {
            NSEventType::KeyDown | NSEventType::KeyUp => {
                let flags = event.modifierFlags();
                if !in_main || flags.contains(NSEventModifierFlags::Command) {
                    return true;
                }
                if self.hub.main_enabled.get()
                    && let Some(key) = keys::key(event.keyCode())
                {
                    let alt = flags.contains(NSEventModifierFlags::Option)
                        && !flags.contains(NSEventModifierFlags::Control);
                    let down = kind == NSEventType::KeyDown;
                    let (message, wparam, lparam) =
                        keys::message(key, down, down && event.isARepeat(), alt);
                    self.hub.push(Event::Input {
                        message,
                        wparam,
                        lparam,
                    });
                }
                // Unhandled keys would only beep.
                false
            }
            NSEventType::FlagsChanged => {
                let flags = event.modifierFlags().0;
                let changed = flags ^ self.modifiers;
                self.modifiers = flags;
                if in_main && self.hub.main_enabled.get() {
                    self.modifier(event.keyCode(), flags, changed);
                }
                true
            }
            NSEventType::LeftMouseDown
            | NSEventType::LeftMouseUp
            | NSEventType::LeftMouseDragged
            | NSEventType::RightMouseDown
            | NSEventType::RightMouseUp
            | NSEventType::RightMouseDragged
            | NSEventType::OtherMouseDown
            | NSEventType::OtherMouseUp
            | NSEventType::OtherMouseDragged
            | NSEventType::MouseMoved
            | NSEventType::ScrollWheel => {
                if in_main {
                    self.mouse(view, event, kind);
                }
                true
            }
            _ => true,
        }
    }

    fn modifier(&mut self, code: u16, flags: usize, changed: usize) {
        let bit = match code {
            0x38 => LEFT_SHIFT,
            0x3c => RIGHT_SHIFT,
            0x3b => LEFT_CONTROL,
            0x3e => RIGHT_CONTROL,
            0x3a => LEFT_ALT,
            0x3d => RIGHT_ALT,
            0x39 => CAPS_LOCK,
            _ => return,
        };
        let Some(key) = keys::key(code) else {
            return;
        };
        if changed & bit == 0 {
            return;
        }
        if self.toggle_control && matches!(bit, LEFT_CONTROL | RIGHT_CONTROL) {
            if flags & bit != 0 {
                self.control_latched = !self.control_latched;
                let (message, wparam, lparam) =
                    keys::message(keys::key(0x3b).unwrap(), self.control_latched, false, false);
                self.hub.push(Event::Input {
                    message,
                    wparam,
                    lparam,
                });
            }
            return;
        }
        let alt = |flags: usize| {
            flags & (LEFT_ALT | RIGHT_ALT) != 0 && flags & (LEFT_CONTROL | RIGHT_CONTROL) == 0
        };
        // Each Caps Lock press toggles the light; it is still one key press.
        let presses: &[bool] = if bit == CAPS_LOCK {
            &[true, false]
        } else if flags & bit != 0 {
            &[true]
        } else {
            &[false]
        };
        for &down in presses {
            let (message, wparam, lparam) = keys::message(key, down, false, alt(flags));
            self.hub.push(Event::Input {
                message,
                wparam,
                lparam,
            });
        }
    }

    /// Top-left client points of an event in the main window.
    fn point(&self, view: &GameView, event: &NSEvent) -> (f64, f64) {
        let location = view.convertPoint_fromView(event.locationInWindow(), None);
        let (_, ch) = view.client();
        (location.x, ch - location.y)
    }

    fn mouse(&mut self, view: &GameView, event: &NSEvent, kind: NSEventType) {
        let (x, y) = self.point(view, event);
        let (cw, ch) = view.client();
        self.update_cursor(view, Some((x, y)));
        if !self.hub.main_enabled.get() {
            return;
        }
        let inside = x >= 0.0 && y >= 0.0 && x < cw && y < ch;
        let viewport = view.viewport((self.width, self.height), self.hub.scaling.get());
        let (gx, gy) = viewport.position(x, y, self.width, self.height);
        let on_canvas = gx >= 0 && gy >= 0 && gx < self.width as i32 && gy < self.height as i32;
        let pressed = NSEvent::pressedMouseButtons();
        let flags = event.modifierFlags();
        let mut buttons = u32::from(pressed & 1 != 0)
            | u32::from(pressed & 2 != 0) << 1
            | u32::from(pressed & 4 != 0) << 4;
        if flags.contains(NSEventModifierFlags::Shift) {
            buttons |= 4;
        }
        if flags.contains(NSEventModifierFlags::Control) {
            buttons |= 8;
        }
        let other = event.buttonNumber() == 2;
        let message = match kind {
            NSEventType::LeftMouseDown => 0x201,
            NSEventType::LeftMouseUp => 0x202,
            NSEventType::RightMouseDown => 0x204,
            NSEventType::RightMouseUp => 0x205,
            NSEventType::OtherMouseDown if other => 0x207,
            NSEventType::OtherMouseUp if other => 0x208,
            NSEventType::ScrollWheel => 0x20a,
            NSEventType::OtherMouseDown | NSEventType::OtherMouseUp => return,
            _ => 0x200,
        };
        // Presses and the wheel need the pointer over the canvas; moves and
        // releases keep going while a drag holds the capture.
        if matches!(message, 0x201 | 0x204 | 0x207 | 0x20a) && !(inside && on_canvas) {
            return;
        }
        if message == 0x200 && !inside && buttons & 0x13 == 0 {
            return;
        }
        let wparam = if message == 0x20a {
            let delta = if event.hasPreciseScrollingDeltas() {
                event.scrollingDeltaY() * 4.0
            } else {
                event.scrollingDeltaY() * 120.0
            };
            self.wheel += delta;
            let notches = (self.wheel / 120.0).trunc();
            if notches == 0.0 {
                return;
            }
            self.wheel -= notches * 120.0;
            let delta = (notches * 120.0).clamp(-32768.0, 32767.0) as i16;
            u32::from(delta as u16) << 16 | (buttons & 0x1f)
        } else {
            buttons
        };
        let position = u32::from(gx as u16) | (u32::from(gy as u16) << 16);
        let (message, wparam) = adv_runtime::mouse_mapping(message, wparam, false);
        self.hub.push(Event::Input {
            message,
            wparam,
            lparam: position,
        });
    }

    fn cursor_visible_at(&self, view: &GameView, point: (f64, f64)) -> bool {
        let viewport = view.viewport((self.width, self.height), self.hub.scaling.get());
        let (x, y) = viewport.position(point.0, point.1, self.width, self.height);
        self.cursor_visible && !adv_runtime::VideoCursorRegion::hidden_at(&self.regions, x, y)
    }

    /// Hides the cursor only over the client area; AppKit counts hides, so
    /// this keeps exactly one outstanding.
    fn update_cursor(&mut self, view: &GameView, point: Option<(f64, f64)>) {
        let point = point.unwrap_or_else(|| {
            let location = view.window().map_or(NSPoint::new(-1.0, -1.0), |window| {
                view.convertPoint_fromView(window.mouseLocationOutsideOfEventStream(), None)
            });
            (location.x, view.client().1 - location.y)
        });
        let (cw, ch) = view.client();
        let inside = point.0 >= 0.0 && point.1 >= 0.0 && point.0 < cw && point.1 < ch;
        let hide = inside
            && view.window().is_some_and(|window| {
                window.isKeyWindow()
                    && window.firstResponder().is_some_and(|responder| {
                        Retained::as_ptr(&responder) as usize == view as *const GameView as usize
                    })
            })
            && !self.cursor_visible_at(view, point);
        if hide != self.cursor_hidden {
            self.cursor_hidden = hide;
            if hide {
                NSCursor::hide();
            } else {
                NSCursor::unhide();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: HOST-SES-8
    #[test]
    fn presentation_never_exceeds_the_guest_refresh_rate() {
        let fast = refresh_interval(180, true);
        assert!(
            fast > Duration::from_secs_f64(1.0 / 70.0)
                && fast < Duration::from_secs_f64(1.0 / 60.0)
        );
        assert_eq!(refresh_interval(120, true), fast);
        assert_eq!(
            refresh_interval(60, true),
            Duration::from_secs_f64(1.0 / 60.0)
        );
        assert_eq!(
            refresh_interval(48, true),
            Duration::from_secs_f64(1.0 / 48.0)
        );
        assert_eq!(
            refresh_interval(0, true),
            Duration::from_secs_f64(1.0 / 24.0)
        );
        assert_eq!(
            refresh_interval(180, false),
            Duration::from_secs_f64(1.0 / 180.0)
        );
    }
    // spec: HOST-RUN-2
    #[test]
    fn toggle_control_ignores_physical_releases_and_uses_one_guest_key() {
        let hub = Rc::new(Hub::default());
        let mut input = Input {
            hub: hub.clone(),
            width: 640,
            height: 480,
            cursor_visible: true,
            cursor_hidden: false,
            regions: Arc::from([]),
            wheel: 0.0,
            modifiers: 0,
            toggle_control: true,
            control_latched: false,
        };
        input.modifier(0x3b, LEFT_CONTROL, LEFT_CONTROL);
        input.modifier(0x3b, 0, LEFT_CONTROL);
        assert!(input.control_latched);
        input.modifier(0x3e, RIGHT_CONTROL, RIGHT_CONTROL);
        input.modifier(0x3e, 0, RIGHT_CONTROL);
        assert!(!input.control_latched);
        let events: Vec<_> = hub.events.borrow_mut().drain(..).collect();
        assert_eq!(events.len(), 2);
        for (event, down) in events.into_iter().zip([true, false]) {
            let Event::Input {
                message,
                wparam,
                lparam,
            } = event
            else {
                panic!("expected Ctrl");
            };
            assert_eq!(
                (message, wparam, lparam),
                keys::message(keys::key(0x3b).unwrap(), down, false, false)
            );
        }
    }
}
