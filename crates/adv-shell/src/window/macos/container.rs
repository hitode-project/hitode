//! Game and overlay views sharing one AppKit content area.
use super::*;
use crate::overlay::{Router, Target};
use objc2::{Message, rc::Weak};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView, NSWindowOrderingMode};
use std::sync::Mutex;

pub struct ContainerState {
    router: Arc<Mutex<Router>>,
    webview: Retained<NSView>,
    game: RefCell<Option<Retained<GameView>>>,
    monitor: RefCell<Option<Retained<AnyObject>>>,
}

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ContainerState]
    pub struct GameContainer;
    unsafe impl NSObjectProtocol for GameContainer {}
    impl GameContainer {
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
            let game = self.ivars().game.borrow();
            if let Some(game) = game.as_ref().filter(|_| hit.is_some()) {
            let local = self.convertPoint_fromView(point, unsafe { self.superview() }.as_deref());
            let pointer = NSApplication::sharedApplication(self.mtm()).currentEvent()
                .filter(|event| matches!(event.r#type(), NSEventType::LeftMouseDown | NSEventType::RightMouseDown | NSEventType::OtherMouseDown | NSEventType::LeftMouseDragged | NSEventType::RightMouseDragged | NSEventType::OtherMouseDragged))
                .map_or(0, |event| event.buttonNumber() as u64);
            match self.ivars().router.lock().unwrap().moved(pointer, local.x, self.bounds().size.height - local.y) {
                Target::Overlay => hit,
                Target::Game => Some(game.clone().into_super()),
            }
            } else { hit }
        }
    }
);

impl GameContainer {
    /// Move the host's existing webview into a full-size container, preserving contentView.
    pub fn new(webview: &NSView, router: Arc<Mutex<Router>>) -> Result<Retained<Self>, String> {
        let mtm = MainThreadMarker::new().ok_or("the container must run on the main thread")?;
        let parent = unsafe { webview.superview() }.ok_or("the webview has no parent")?;
        let this = Self::alloc(mtm).set_ivars(ContainerState {
            router,
            webview: webview.retain(),
            game: RefCell::new(None),
            monitor: RefCell::new(None),
        });
        let view: Retained<Self> =
            unsafe { msg_send![super(this), initWithFrame: parent.bounds()] };
        let mask = NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewHeightSizable;
        view.setAutoresizingMask(mask);
        parent.addSubview(&view);
        webview.removeFromSuperview();
        view.addSubview(webview);
        webview.setFrame(view.bounds());
        webview.setAutoresizingMask(mask);
        // AppKit keeps the mouse responder after the down hit test. Observe
        // down/up here so Router sees the release even without another hitTest:.
        let weak = Weak::from_retained(&view);
        let block = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| {
            let event_ref = unsafe { event.as_ref() };
            if let Some(view) = weak.load()
                && event_ref.window(mtm) == view.window()
            {
                let point = view.convertPoint_fromView(event_ref.locationInWindow(), None);
                let (x, y) = (point.x, view.bounds().size.height - point.y);
                let mut router = view.ivars().router.lock().unwrap();
                let pointer = event_ref.buttonNumber() as u64;
                match event_ref.r#type() {
                    NSEventType::LeftMouseDown
                    | NSEventType::RightMouseDown
                    | NSEventType::OtherMouseDown => {
                        router.down(pointer, x, y);
                    }
                    NSEventType::LeftMouseUp
                    | NSEventType::RightMouseUp
                    | NSEventType::OtherMouseUp => {
                        router.up(pointer, x, y);
                    }
                    _ => unreachable!(),
                }
            }
            event.as_ptr()
        });
        *view.ivars().monitor.borrow_mut() = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::LeftMouseDown
                    | NSEventMask::LeftMouseUp
                    | NSEventMask::RightMouseDown
                    | NSEventMask::RightMouseUp
                    | NSEventMask::OtherMouseDown
                    | NSEventMask::OtherMouseUp,
                &block,
            )
        };
        Ok(view)
    }

    /// Mount below the webview, or remove the completed game's view.
    pub fn set_game(&self, game: Option<&GameView>) {
        if let Some(old) = self.ivars().game.borrow_mut().take() {
            old.removeFromSuperview();
        }
        if let Some(game) = game {
            game.setFrame(self.bounds());
            game.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            self.addSubview_positioned_relativeTo(
                game,
                NSWindowOrderingMode::Below,
                Some(&self.ivars().webview),
            );
            *self.ivars().game.borrow_mut() = Some(game.retain());
        }
    }
}

impl Drop for ContainerState {
    fn drop(&mut self) {
        if let Some(monitor) = self.monitor.get_mut().take() {
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
    }
}
