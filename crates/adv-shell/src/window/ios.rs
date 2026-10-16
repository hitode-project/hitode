//! The iOS game view: a black `UIView` whose frame sublayer is the
//! presentation `CAMetalLayer`, forwarding touches and hardware keys to the
//! shared mobile input policy. The host inserts it below its transparent
//! webview. As on macOS, frames render at canvas size and Core Animation
//! scales the layer, so no display-resolution pass runs (HOST-VIEW-9).
use super::GameView;
pub use crate::mobile::TouchAction;
pub(crate) use crate::mobile::{Touch, key_message as message, position};
use crate::overlay::{Router, Target};
use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSObjectProtocol, NSSet};
use objc2_quartz_core::{CAMetalLayer, CATransaction};
use objc2_ui_kit::UIViewAutoresizing;
use objc2_ui_kit::{
    UIColor, UIEvent, UIPress, UIPressesEvent, UIResponder, UITouch, UITouchPhase, UIView,
};
use std::cell::{Cell, RefCell};
use std::rc::Weak;
use std::sync::{Arc, Mutex};

pub struct Ivars {
    game: Weak<GameView>,
    frame: Retained<CAMetalLayer>,
    /// The frame's last placement in pixels, kept across layouts.
    placement: Cell<Option<crate::native_gpu::Placement>>,
    /// Live touches by object address, with the pointer ids the mobile
    /// policy tracks; UIKit reuses one `UITouch` for a finger's lifetime.
    touches: RefCell<Vec<(usize, i32)>>,
    next_pointer: Cell<i32>,
}

define_class!(
    #[unsafe(super(UIView, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct HostView;
    unsafe impl NSObjectProtocol for HostView {}
    impl HostView {
        #[unsafe(method(layoutSubviews))]
        fn layout_subviews(&self) {
            unsafe { let _: () = msg_send![super(self), layoutSubviews]; }
            self.layout();
        }
        #[unsafe(method(didMoveToWindow))]
        fn did_move_to_window(&self) {
            self.layout();
        }
        #[unsafe(method(canBecomeFirstResponder))]
        fn can_become_first_responder(&self) -> bool {
            true
        }
        #[unsafe(method(touchesBegan:withEvent:))]
        fn touches_began(&self, touches: &NSSet<UITouch>, _: Option<&UIEvent>) {
            self.touches(touches);
        }
        #[unsafe(method(touchesMoved:withEvent:))]
        fn touches_moved(&self, touches: &NSSet<UITouch>, _: Option<&UIEvent>) {
            self.touches(touches);
        }
        #[unsafe(method(touchesEnded:withEvent:))]
        fn touches_ended(&self, touches: &NSSet<UITouch>, _: Option<&UIEvent>) {
            self.touches(touches);
        }
        #[unsafe(method(touchesCancelled:withEvent:))]
        fn touches_cancelled(&self, touches: &NSSet<UITouch>, _: Option<&UIEvent>) {
            self.touches(touches);
        }
        #[unsafe(method(pressesBegan:withEvent:))]
        fn presses_began(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            if !self.presses(presses, true) {
                unsafe { let _: () = msg_send![super(self), pressesBegan: presses, withEvent: event]; }
            }
        }
        #[unsafe(method(pressesEnded:withEvent:))]
        fn presses_ended(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            if !self.presses(presses, false) {
                unsafe { let _: () = msg_send![super(self), pressesEnded: presses, withEvent: event]; }
            }
        }
        #[unsafe(method(pressesCancelled:withEvent:))]
        fn presses_cancelled(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            if !self.presses(presses, false) {
                unsafe { let _: () = msg_send![super(self), pressesCancelled: presses, withEvent: event]; }
            }
        }
    }
);

impl HostView {
    pub(crate) fn new(game: Weak<GameView>) -> Result<Retained<Self>, String> {
        let mtm = MainThreadMarker::new().ok_or("the game view must run on the main thread")?;
        let frame = CAMetalLayer::new();
        let this = Self::alloc(mtm).set_ivars(Ivars {
            game,
            frame: frame.clone(),
            placement: Cell::new(None),
            touches: RefCell::new(Vec::new()),
            next_pointer: Cell::new(0),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.setMultipleTouchEnabled(true);
        this.setOpaque(true);
        this.setBackgroundColor(Some(&UIColor::blackColor()));
        this.layer().addSublayer(&frame);
        Ok(this)
    }
    /// The frame sublayer the presenter draws into.
    pub(crate) fn metal_layer(&self) -> *mut std::ffi::c_void {
        Retained::as_ptr(&self.ivars().frame).cast_mut().cast()
    }
    /// Positions the frame layer; None fills the view.
    pub(crate) fn place(&self, placement: Option<crate::native_gpu::Placement>) {
        self.ivars().placement.set(placement);
        let scale = self.contentScaleFactor();
        let bounds = self.bounds().size;
        let (rectangle, nearest) = match placement {
            Some(placement) => (
                placement.rectangle.map(|v| f64::from(v) / scale),
                placement.nearest,
            ),
            None => ([0.0, 0.0, bounds.width, bounds.height], false),
        };
        let [x, y, w, h] = rectangle;
        let frame = &self.ivars().frame;
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        frame.setFrame(CGRect::new(CGPoint::new(x, y), CGSize::new(w, h)));
        unsafe {
            frame.setMagnificationFilter(if nearest {
                objc2_quartz_core::kCAFilterNearest
            } else {
                objc2_quartz_core::kCAFilterLinear
            });
        }
        CATransaction::commit();
    }
    /// Pixels map one to one onto the drawable.
    fn layout(&self) {
        let scale = self
            .window()
            .map_or(self.contentScaleFactor(), |window| window.screen().scale());
        self.setContentScaleFactor(scale);
        self.layer().setContentsScale(scale);
        self.ivars().frame.setContentsScale(scale);
        self.place(self.ivars().placement.get());
        let size = self.bounds().size;
        let pixels = |points: f64| (points * scale).round().max(0.0) as u32;
        if let Some(game) = self.ivars().game.upgrade() {
            game.resized(pixels(size.width), pixels(size.height));
        }
    }
    fn touches(&self, touches: &NSSet<UITouch>) {
        let Some(game) = self.ivars().game.upgrade() else {
            return;
        };
        let scale = self.contentScaleFactor();
        for touch in touches.iter() {
            let key = Retained::as_ptr(&touch) as usize;
            let action = match touch.phase() {
                UITouchPhase::Began => TouchAction::Down,
                UITouchPhase::Moved | UITouchPhase::Stationary => TouchAction::Move,
                UITouchPhase::Ended => TouchAction::Up,
                UITouchPhase::Cancelled => TouchAction::Cancel,
                _ => continue,
            };
            let pointer = {
                let mut live = self.ivars().touches.borrow_mut();
                match live.iter().find(|(k, _)| *k == key) {
                    Some(&(_, pointer)) => pointer,
                    None => {
                        let pointer = self.ivars().next_pointer.get();
                        self.ivars().next_pointer.set(pointer.wrapping_add(1));
                        live.push((key, pointer));
                        pointer
                    }
                }
            };
            if matches!(action, TouchAction::Up | TouchAction::Cancel) {
                self.ivars().touches.borrow_mut().retain(|(k, _)| *k != key);
            }
            let at = touch.locationInView(Some(self));
            game.touch(
                pointer,
                action,
                (at.x * scale) as f32,
                (at.y * scale) as f32,
            );
        }
    }
    /// True when every press was a key the game takes.
    fn presses(&self, presses: &NSSet<UIPress>, down: bool) -> bool {
        let Some(game) = self.ivars().game.upgrade() else {
            return false;
        };
        let mut handled = true;
        for press in presses.iter() {
            match press
                .key(MainThreadMarker::from(self))
                .map(|key| key.keyCode().0)
            {
                Some(code) if key(code as i32).is_some() => game.key(code as i32, down),
                _ => handled = false,
            }
        }
        handled
    }
}

/// USB HID keyboard usages (`UIKeyboardHIDUsage`) to Win32 virtual keys and
/// set-1 scan codes.
pub(crate) fn key(code: i32) -> Option<(u32, u32)> {
    const LETTERS: [u32; 26] = [
        0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
    ];
    Some(match code {
        0x04..=0x1d => (0x41 + (code - 0x04) as u32, LETTERS[(code - 0x04) as usize]),
        0x1e..=0x26 => (0x31 + (code - 0x1e) as u32, (code - 0x1e + 2) as u32),
        0x27 => (0x30, 0x0b),
        0x28 => (0x0d, 0x1c),
        0x29 => (0x1b, 0x01),
        0x2a => (0x08, 0x0e),
        0x2b => (0x09, 0x0f),
        0x2c => (0x20, 0x39),
        0x2d => (0xbd, 0x0c),
        0x2e => (0xbb, 0x0d),
        0x2f => (0xdb, 0x1a),
        0x30 => (0xdd, 0x1b),
        0x31 => (0xdc, 0x2b),
        0x33 => (0xba, 0x27),
        0x34 => (0xde, 0x28),
        0x35 => (0xc0, 0x29),
        0x36 => (0xbc, 0x33),
        0x37 => (0xbe, 0x34),
        0x38 => (0xbf, 0x35),
        0x39 => (0x14, 0x3a),
        0x3a..=0x43 => (0x70 + (code - 0x3a) as u32, (code - 0x3a + 0x3b) as u32),
        0x44 => (0x7a, 0x57),
        0x45 => (0x7b, 0x58),
        0x46 => (0x2c, 0x137),
        0x47 => (0x91, 0x46),
        0x48 => (0x13, 0x45),
        0x49 => (0x2d, 0x152),
        0x4a => (0x24, 0x147),
        0x4b => (0x21, 0x149),
        0x4c => (0x2e, 0x153),
        0x4d => (0x23, 0x14f),
        0x4e => (0x22, 0x151),
        0x4f => (0x27, 0x14d),
        0x50 => (0x25, 0x14b),
        0x51 => (0x28, 0x150),
        0x52 => (0x26, 0x148),
        0xe0 => (0x11, 0x1d),
        0xe1 => (0x10, 0x2a),
        0xe2 => (0x12, 0x38),
        0xe4 => (0x11, 0x11d),
        0xe5 => (0x10, 0x36),
        0xe6 => (0x12, 0x138),
        _ => return None,
    })
}

pub struct ContainerState {
    router: Arc<Mutex<Router>>,
    webview: Retained<UIView>,
    game: RefCell<Option<Retained<HostView>>>,
}

define_class!(
    /// The host's webview and the game view below it, sharing the host's
    /// content area. Each new touch goes to the game unless the overlay
    /// claims its point; UIKit keeps that target for the touch's lifetime.
    #[unsafe(super(UIView, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ContainerState]
    pub struct GameContainer;
    unsafe impl NSObjectProtocol for GameContainer {}
    impl GameContainer {
        #[unsafe(method_id(hitTest:withEvent:))]
        fn hit_test(&self, point: CGPoint, event: Option<&UIEvent>) -> Option<Retained<UIView>> {
            let hit: Option<Retained<UIView>> =
                unsafe { msg_send![super(self), hitTest: point, withEvent: event] };
            let game = self.ivars().game.borrow();
            match game.as_ref().filter(|_| hit.is_some()) {
                Some(game) => match self.ivars().router.lock().unwrap().input().target_at(point.x, point.y) {
                    Target::Overlay => hit,
                    Target::Game => Some(game.clone().into_super()),
                },
                None => hit,
            }
        }
    }
);

impl GameContainer {
    /// Moves the host's webview into a full-size container in its place.
    pub fn new(webview: &UIView, router: Arc<Mutex<Router>>) -> Result<Retained<Self>, String> {
        let mtm = MainThreadMarker::new().ok_or("the container must run on the main thread")?;
        let parent = webview.superview().ok_or("the webview has no parent")?;
        let this = Self::alloc(mtm).set_ivars(ContainerState {
            router,
            webview: webview.retain(),
            game: RefCell::new(None),
        });
        let view: Retained<Self> =
            unsafe { msg_send![super(this), initWithFrame: parent.bounds()] };
        let mask = UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight;
        view.setAutoresizingMask(mask);
        view.setBackgroundColor(Some(&UIColor::blackColor()));
        parent.addSubview(&view);
        webview.removeFromSuperview();
        view.addSubview(webview);
        webview.setFrame(view.bounds());
        webview.setAutoresizingMask(mask);
        webview.setOpaque(false);
        webview.setBackgroundColor(Some(&UIColor::clearColor()));
        Ok(view)
    }

    /// Hides the webview over a running game while the overlay draws
    /// nothing, taking it out of the composition.
    pub fn set_overlay_visible(&self, visible: bool) {
        let game = self.ivars().game.borrow().is_some();
        self.ivars().webview.setHidden(game && !visible);
    }

    /// Mounts the game view below the webview, or removes the finished one.
    pub fn set_game(&self, game: Option<&GameView>) {
        if let Some(old) = self.ivars().game.borrow_mut().take() {
            old.removeFromSuperview();
        }
        self.ivars().webview.setHidden(false);
        let Some(host) = game.and_then(|game| game.host.borrow().clone()) else {
            return;
        };
        host.setFrame(self.bounds());
        host.setAutoresizingMask(
            UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight,
        );
        self.insertSubview_belowSubview(&host, &self.ivars().webview);
        host.becomeFirstResponder();
        *self.ivars().game.borrow_mut() = Some(host);
    }
}
