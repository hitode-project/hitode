//! The window's title bar while the game screen is shown (macOS).
//!
//! The library draws under a transparent title bar. Over a game that strip
//! covers the game's top edge and leaves nothing to drag the window by, so
//! the game screen gets an opaque title bar with the game's title and the
//! pause button, which then never covers the game. Full screen hides the bar
//! until the pointer reaches the top of the screen.
//!
//! The window itself stays opaque on every screen: a transparent window is
//! blended with the desktop behind it, and WindowServer redid that for every
//! game frame (about half a core for AQUA's title screen) and kept the GPU
//! awake over an idle library. Only the webview is transparent, for the game
//! overlay; the library paints its own background.
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject},
    sel,
};
use objc2_app_kit::{
    NSButton, NSColor, NSImage, NSLayoutAttribute, NSTitlebarAccessoryViewController, NSWindow,
    NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize, ns_string};
use std::cell::RefCell;
use tauri::{Manager, WebviewWindow};

define_class!(
    /// Emits the same `pause` event as the ⌘. menu item.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = tauri::AppHandle]
    struct PauseTarget;
    unsafe impl NSObjectProtocol for PauseTarget {}
    impl PauseTarget {
        #[unsafe(method(pause:))]
        fn pause(&self, _sender: &AnyObject) {
            crate::game::pause(self.ivars());
        }
    }
);

type Accessory = (
    Retained<NSTitlebarAccessoryViewController>,
    Retained<PauseTarget>,
);

thread_local! {
    static PAUSE: RefCell<Option<Accessory>> = const { RefCell::new(None) };
}

/// Makes the main window opaque from launch; the config's `transparent` is
/// for the webview.
pub fn setup(app: &tauri::App) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("main") {
        let native = window.ns_window()?;
        // SAFETY: Tauri keeps the NSWindow alive while its WebviewWindow exists.
        paint(unsafe { &*native.cast::<NSWindow>() }, false);
    }
    Ok(())
}

fn paint(native: &NSWindow, game: bool) {
    native.setOpaque(true);
    let background = if game {
        NSColor::blackColor()
    } else {
        NSColor::windowBackgroundColor()
    };
    native.setBackgroundColor(Some(&background));
}

/// `title` is the game's while the game screen is shown, null after it.
#[tauri::command]
pub fn set_title_bar(window: WebviewWindow, title: Option<String>) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("the title bar changes on the main thread")?;
    let native = window.ns_window().map_err(|e| e.to_string())?;
    // SAFETY: Tauri keeps the NSWindow alive while its WebviewWindow exists.
    let native = unsafe { &*native.cast::<NSWindow>() };
    let game = title.is_some();
    let mut mask = native.styleMask();
    mask.set(NSWindowStyleMask::FullSizeContentView, !game);
    native.setStyleMask(mask);
    native.setTitlebarAppearsTransparent(!game);
    paint(native, game);
    native.setTitleVisibility(if game {
        NSWindowTitleVisibility::Visible
    } else {
        NSWindowTitleVisibility::Hidden
    });
    let name = window.app_handle().package_info().name.clone();
    window
        .set_title(title.as_deref().unwrap_or(&name))
        .map_err(|e| e.to_string())?;
    PAUSE.with_borrow_mut(|pause| {
        if let Some((accessory, _)) = pause.take() {
            accessory.removeFromParentViewController();
        }
        if !game {
            return;
        }
        let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            ns_string!("pause.fill"),
            Some(ns_string!("暂停")),
        ) else {
            return;
        };
        let target = PauseTarget::alloc(mtm).set_ivars(window.app_handle().clone());
        let target: Retained<PauseTarget> = unsafe { msg_send![super(target), init] };
        // SAFETY: the target is retained with the accessory, as long as the button.
        let button = unsafe {
            NSButton::buttonWithImage_target_action(&image, Some(&target), Some(sel!(pause:)), mtm)
        };
        button.setBordered(false);
        button.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(40.0, 28.0)));
        let accessory = NSTitlebarAccessoryViewController::new(mtm);
        accessory.setView(&button);
        accessory.setLayoutAttribute(NSLayoutAttribute::Right);
        native.addTitlebarAccessoryViewController(&accessory);
        *pause = Some((accessory, target));
    });
    Ok(())
}
