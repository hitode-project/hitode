//! The macOS window on real AppKit. AppKit needs the main thread, so this
//! test owns `main` (harness = false) and runs each check in order.
//! Usage: `cargo test -p adv-shell --test macos_window`; it opens hidden
//! windows only and prints one line per check.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    mac::run();
}

#[cfg(target_os = "macos")]
mod mac {
    use adv_runtime::{
        DialogControl, DialogEvent, DialogSet, DialogTarget, DialogWindow, MenuBar, MenuCommand,
        MenuEvent, MenuItem, MenuKey, Snapshot,
    };
    use adv_shell::window::{Event, WaitOutcome, WakeEvent, Window};
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{MainThreadMarker, msg_send};
    use objc2_app_kit::{
        NSApplication, NSButton, NSEvent, NSEventModifierFlags, NSEventType, NSPopUpButton,
        NSTextField, NSView, NSWindow,
    };
    use objc2_foundation::{NSPoint, NSString};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    fn inputs(events: Vec<Event>) -> Vec<(u32, u32, u32)> {
        events
            .into_iter()
            .filter_map(|event| match event {
                Event::Input {
                    message,
                    wparam,
                    lparam,
                } => Some((message, wparam, lparam)),
                _ => None,
            })
            .collect()
    }

    fn position(x: i32, y: i32) -> u32 {
        u32::from(x as u16) | (u32::from(y as u16) << 16)
    }

    fn find_window(mtm: MainThreadMarker, title: &str) -> Retained<NSWindow> {
        NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .find(|window| window.title().to_string() == title)
            .unwrap_or_else(|| panic!("no window titled {title}"))
    }

    fn key_event(
        window: &NSWindow,
        kind: NSEventType,
        code: u16,
        flags: NSEventModifierFlags,
    ) -> Retained<NSEvent> {
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind,
            NSPoint::new(0.0, 0.0),
            flags,
            0.0,
            window.windowNumber(),
            None,
            &NSString::from_str("a"),
            &NSString::from_str("a"),
            false,
            code,
        )
        .unwrap()
    }

    fn post(mtm: MainThreadMarker, event: &NSEvent) {
        NSApplication::sharedApplication(mtm).postEvent_atStart(event, false);
    }

    fn subviews(window: &NSWindow) -> Vec<Retained<NSView>> {
        window.contentView().unwrap().subviews().to_vec()
    }

    pub fn run() {
        let mtm = MainThreadMarker::new().expect("main thread");
        let mut window = Window::new("adv macos window test", 320, 240, true).unwrap();
        let native = find_window(mtm, "adv macos window test");

        // Presenting sizes the content to the canvas in points.
        let frame = Snapshot {
            width: 320,
            height: 240,
            bgra: vec![0x80; 320 * 240 * 4],
            frames: 1,
        };
        window.present(&frame).unwrap();
        let size = native.contentView().unwrap().bounds().size;
        assert_eq!((size.width, size.height), (320.0, 240.0));
        println!("ok present sizes the canvas");

        // Keys: positions become VK, scan code and the Win32 lParam layout.
        window.pump();
        post(
            mtm,
            &key_event(
                &native,
                NSEventType::KeyDown,
                0x00,
                NSEventModifierFlags::empty(),
            ),
        );
        post(
            mtm,
            &key_event(
                &native,
                NSEventType::KeyUp,
                0x00,
                NSEventModifierFlags::empty(),
            ),
        );
        post(
            mtm,
            &key_event(
                &native,
                NSEventType::KeyDown,
                0x7b,
                NSEventModifierFlags::empty(),
            ),
        );
        // Command shortcuts stay with macOS.
        post(
            mtm,
            &key_event(
                &native,
                NSEventType::KeyDown,
                0x0c,
                NSEventModifierFlags::Command,
            ),
        );
        // Option without Control makes a system key.
        post(
            mtm,
            &key_event(
                &native,
                NSEventType::KeyDown,
                0x24,
                NSEventModifierFlags::Option,
            ),
        );
        assert_eq!(
            inputs(window.pump()),
            [
                (0x100, 0x41, 0x001e_0001),
                (0x101, 0x41, 0xc01e_0001),
                (0x100, 0x25, 0x014b_0001),
                (0x104, 0x0d, 0x201c_0001),
            ]
        );
        println!("ok keys");

        // Mouse: a click at canvas pixel (160, 60); AppKit's y is bottom-up.
        let click = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDown,
            NSPoint::new(160.0, 180.0),
            NSEventModifierFlags::empty(),
            0.0,
            native.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
        .unwrap();
        post(mtm, &click);
        let mouse = inputs(window.pump());
        assert!(
            mouse
                .iter()
                .any(|&(m, _, l)| m == 0x201 && l == position(160, 60)),
            "{mouse:x?}"
        );
        // Posted client points map through the same viewport.
        window.post_input(0x200, 0, position(10, 20)).unwrap();
        assert_eq!(inputs(window.pump()), [(0x200, 0, position(10, 20))]);
        println!("ok mouse");

        // Waits end at the deadline, or early on a wake from another thread.
        let wake = WakeEvent::new().unwrap();
        let started = Instant::now();
        assert_eq!(
            window
                .wait_with_wake(Some(Duration::from_millis(30)), Some(&wake))
                .unwrap(),
            WaitOutcome::Deadline
        );
        assert!(started.elapsed() >= Duration::from_millis(25));
        let signal = wake.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            signal.signal();
        });
        let started = Instant::now();
        assert_eq!(
            window
                .wait_with_wake(Some(Duration::from_secs(5)), Some(&wake))
                .unwrap(),
            WaitOutcome::Input
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        thread.join().unwrap();
        window.pump();
        // The wake was consumed. AppKit may still post its own events, so
        // the pipe is checked rather than another wait.
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
        println!("ok wake");

        // Menus: the guest bar follows the application menu.
        let menus = Rc::new(RefCell::new(Vec::new()));
        let sink = menus.clone();
        window.set_menu_event_sink(Box::new(move |event| {
            sink.borrow_mut().push(event);
            Ok(())
        }));
        let key = MenuKey {
            window: 0x100123,
            menu: 0x7000004,
            revision: 1,
            enabled: true,
        };
        let command = MenuCommand {
            window: key.window,
            root: key.menu,
            menu: 0x7000008,
            position: 0,
            id: 0x10404,
        };
        let leaf = MenuItem {
            text: "オートプレイ\tF6".encode_utf16().collect(),
            flags: 8,
            supported: true,
            command,
            submenu: 0,
            children: Vec::new(),
        };
        let mut disabled = leaf.clone();
        disabled.flags = 3;
        disabled.command.id = 77;
        let bar = MenuBar {
            key,
            popup: None,
            items: vec![MenuItem {
                text: "&設定".encode_utf16().collect(),
                flags: 0,
                supported: true,
                command: MenuCommand::default(),
                submenu: command.menu,
                children: vec![leaf, disabled],
            }],
        };
        window.set_menu_bar(&bar).unwrap();
        let main = NSApplication::sharedApplication(mtm).mainMenu().unwrap();
        assert_eq!(main.numberOfItems(), 2);
        let top = main.itemAtIndex(1).unwrap();
        assert_eq!(top.title().to_string(), "設定");
        let submenu = top.submenu().unwrap();
        assert_eq!(
            submenu.itemAtIndex(0).unwrap().title().to_string(),
            "オートプレイ"
        );
        assert_eq!(submenu.itemAtIndex(0).unwrap().state(), 1);
        assert!(!submenu.itemAtIndex(1).unwrap().isEnabled());
        let delegate = submenu.delegate().unwrap();
        let _: () = unsafe { msg_send![&*delegate, menuWillOpen: &*submenu] };
        submenu.performActionForItemAtIndex(0);
        let _: () = unsafe { msg_send![&*delegate, menuDidClose: &*submenu] };
        assert_eq!(
            *menus.borrow(),
            [
                MenuEvent::Initialize {
                    window: key.window,
                    root: key.menu,
                    menu: key.menu,
                    position: None
                },
                MenuEvent::Initialize {
                    window: key.window,
                    root: key.menu,
                    menu: command.menu,
                    position: Some(0)
                },
                MenuEvent::Command(command),
            ]
        );
        window.set_menu_bar(&MenuBar::default()).unwrap();
        assert_eq!(main.numberOfItems(), 1);
        println!("ok menus");

        // Dialogs: native controls report guest events and keep guest state.
        let dialogs = Rc::new(RefCell::new(Vec::new()));
        let sink = dialogs.clone();
        window.set_dialog_event_sink(Box::new(move |event| {
            sink.borrow_mut().push(event);
            Ok(())
        }));
        let target = |hwnd: u32, id: u32| DialogTarget {
            dialog: 0x200,
            hwnd,
            parent: 0x200,
            id,
            created_us: 1,
        };
        let control = |hwnd: u32, class: &str, style: u32, text: &str, top: i32| {
            let mut control = DialogControl {
                target: target(hwnd, hwnd & 0xff),
                class: class.into(),
                style,
                ex_style: 0,
                text: text.into(),
                rect: Default::default(),
                visible: true,
                enabled: true,
                check: 0,
                items: Vec::new(),
                selection: -1,
                range: None,
                value: 0,
                font: None,
                supported: true,
            };
            control.rect.left = 10;
            control.rect.top = top;
            control.rect.right = 190;
            control.rect.bottom = top + 24;
            control
        };
        let ok = control(0x201, "Button", 1, "OK", 10);
        let mut check = control(0x202, "Button", 2, "Fullscreen", 40);
        check.check = 1;
        let edit = control(0x203, "Edit", 0, "Name", 70);
        let mut combo = control(0x204, "ComboBox", 3, "", 100);
        combo.items = vec!["800x600".into(), "1280x720".into()];
        combo.selection = 0;
        let dialog = DialogWindow {
            target: DialogTarget {
                dialog: 0x200,
                hwnd: 0x200,
                parent: 0,
                id: 0,
                created_us: 1,
            },
            owner: 0x100123,
            owner_enabled: false,
            class: "#32770".into(),
            style: 0x8_0000,
            ex_style: 0,
            text: "Settings".into(),
            rect: Default::default(),
            client_size: (200, 140),
            visible: true,
            enabled: true,
            modal: true,
            focus: 0x203,
            font: None,
            controls: vec![ok, check, edit, combo],
        };
        let set = DialogSet {
            dialogs: vec![dialog],
        };
        window.set_dialogs(&set).unwrap();
        let panel = find_window(mtm, "Settings");
        let views = subviews(&panel);
        assert_eq!(views.len(), 4);
        let button = views[0].downcast_ref::<NSButton>().unwrap();
        unsafe { button.performClick(None) };
        let checkbox = views[1].downcast_ref::<NSButton>().unwrap();
        unsafe { checkbox.performClick(None) };
        // The guest's check survives the native toggle.
        assert_eq!(checkbox.state(), 1);
        let field = views[2].downcast_ref::<NSTextField>().unwrap();
        field.setStringValue(&NSString::from_str("Alice"));
        let delegate = field.delegate().unwrap();
        let notification = unsafe {
            objc2_foundation::NSNotification::notificationWithName_object(
                &NSString::from_str("NSControlTextDidChangeNotification"),
                Some(field as &AnyObject),
            )
        };
        let _: () = unsafe { msg_send![&*delegate, controlTextDidChange: &*notification] };
        let popup = views[3].downcast_ref::<NSPopUpButton>().unwrap();
        assert_eq!(popup.numberOfItems(), 2);
        popup.selectItemAtIndex(1);
        let _ = unsafe { popup.sendAction_to(popup.action(), popup.target().as_deref()) };
        // Tab belongs to the guest dialog.
        post(
            mtm,
            &key_event(
                &panel,
                NSEventType::KeyDown,
                0x30,
                NSEventModifierFlags::empty(),
            ),
        );
        window.pump();
        panel.performClose(None);
        let events = dialogs.borrow().clone();
        let has = |wanted: &DialogEvent| events.iter().any(|event| event == wanted);
        assert!(has(&DialogEvent::Click(target(0x201, 1))), "{events:?}");
        assert!(has(&DialogEvent::Click(target(0x202, 2))), "{events:?}");
        assert!(has(&DialogEvent::Text {
            target: target(0x203, 3),
            text: "Alice".into()
        }));
        assert!(has(&DialogEvent::Select {
            target: target(0x204, 4),
            index: 1
        }));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, DialogEvent::Key { key: 9, .. })),
            "{events:?}"
        );
        assert!(has(&DialogEvent::Close(set.dialogs[0].target)));
        // A modal dialog owned by the main window blocks canvas input.
        window.pump();
        post(
            mtm,
            &key_event(
                &native,
                NSEventType::KeyDown,
                0x00,
                NSEventModifierFlags::empty(),
            ),
        );
        assert!(inputs(window.pump()).is_empty());
        window.set_dialogs(&DialogSet::default()).unwrap();
        assert!(!panel.isVisible());
        post(
            mtm,
            &key_event(
                &native,
                NSEventType::KeyDown,
                0x00,
                NSEventModifierFlags::empty(),
            ),
        );
        assert_eq!(inputs(window.pump()).len(), 1);
        println!("ok dialogs");

        drop(window);
        println!("ok all");
    }
}
