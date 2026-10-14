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
        MenuEvent, MenuItem, MenuKey,
    };
    use adv_shell::window::{Event, Window};
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{MainThreadMarker, msg_send};
    use objc2_app_kit::{
        NSApplication, NSButton, NSEvent, NSEventModifierFlags, NSEventType, NSPopUpButton,
        NSTextField, NSView, NSWindow,
    };
    use objc2_core_foundation::{
        CFAbsoluteTimeGetCurrent, CFRunLoop, CFRunLoopTimer, CFRunLoopTimerContext,
        kCFRunLoopCommonModes,
    };
    use objc2_foundation::{NSNotification, NSPoint, NSString};
    use std::cell::RefCell;
    use std::rc::Rc;

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
        if let Some(view) = event.window(mtm).and_then(|window| window.contentView()) {
            if view.downcast_ref::<adv_shell::window::GameView>().is_some() {
                match event.r#type() {
                    NSEventType::KeyDown => view.keyDown(event),
                    NSEventType::KeyUp => view.keyUp(event),
                    NSEventType::LeftMouseDown => view.mouseDown(event),
                    _ => unreachable!(),
                }
                return;
            }
        }
        NSApplication::sharedApplication(mtm).sendEvent(event);
    }

    fn subviews(window: &NSWindow) -> Vec<Retained<NSView>> {
        window.contentView().unwrap().subviews().to_vec()
    }

    fn dispatch(window: &mut Window) -> Vec<Event> {
        unsafe extern "C-unwind" fn stop(_: *mut CFRunLoopTimer, _: *mut std::ffi::c_void) {
            NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).stop(None);
            // AppKit needs an event to finish the current dispatch after stop.
            let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                NSEventType::ApplicationDefined, NSPoint::new(0.0, 0.0), NSEventModifierFlags::empty(), 0.0, 0, None, 0, 0, 0).unwrap();
            NSApplication::sharedApplication(MainThreadMarker::new().unwrap())
                .postEvent_atStart(&event, false);
        }
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let timer = unsafe {
            CFRunLoopTimer::new(
                None,
                CFAbsoluteTimeGetCurrent() + 0.005,
                0.0,
                0,
                0,
                Some(stop),
                &mut context,
            )
        }
        .unwrap();
        CFRunLoop::main()
            .unwrap()
            .add_timer(Some(&timer), unsafe { kCFRunLoopCommonModes });
        NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).run();
        window.pump()
    }

    // spec: HOST-VIEW-6
    pub fn run() {
        let mtm = MainThreadMarker::new().expect("main thread");
        let mut window = Window::new("adv macos window test", 320, 240, true).unwrap();
        let native = find_window(mtm, "adv macos window test");

        let (gpu, presenter) = window.create_native(false).unwrap();
        window.native_canvas((320, 240)).unwrap();
        let size = native.contentView().unwrap().bounds().size;
        let scale = native.backingScaleFactor();
        assert_eq!((size.width, size.height), (320.0, 240.0));
        assert_eq!(
            window.native_client_size().unwrap(),
            ((320.0 * scale) as u32, (240.0 * scale) as u32)
        );
        window.native_canvas((1600, 900)).unwrap();
        assert_eq!(
            window.native_client_size().unwrap(),
            ((1600.0 * scale) as u32, (900.0 * scale) as u32)
        );
        window.native_canvas((320, 240)).unwrap();
        drop(presenter);
        drop(gpu);
        println!("ok Metal surface and backing pixel dimensions");

        // Keys: positions become VK, scan code and the Win32 lParam layout.
        dispatch(&mut window);
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
            inputs(dispatch(&mut window)),
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
        let mouse = inputs(dispatch(&mut window));
        assert!(
            mouse
                .iter()
                .any(|&(m, _, l)| m == 0x201 && l == position(160, 60)),
            "{mouse:x?}"
        );
        // Posted client points map through the same viewport.
        window.post_input(0x200, 0, position(10, 20)).unwrap();
        assert_eq!(
            inputs(dispatch(&mut window)),
            [(0x200, 0, position(10, 20))]
        );
        println!("ok mouse");

        // The embedded container routes top-left logical points, while Router
        // owns captures even when regions change between mouse down and up.
        {
            use adv_shell::overlay::{OverlayInput, Rect, Router, Target};
            use adv_shell::window::{GameContainer, GameView};
            use objc2::{MainThreadOnly, msg_send};
            use std::sync::{Arc, Mutex};
            let game = native
                .contentView()
                .unwrap()
                .downcast::<GameView>()
                .unwrap();
            let root: Retained<NSView> =
                unsafe { msg_send![NSView::alloc(mtm), initWithFrame: game.frame()] };
            let overlay: Retained<NSView> =
                unsafe { msg_send![NSView::alloc(mtm), initWithFrame: root.bounds()] };
            native.setContentView(Some(&root));
            root.addSubview(&overlay);
            let router = Arc::new(Mutex::new(Router::new(OverlayInput {
                regions: vec![Rect {
                    x: 10.0,
                    y: 10.0,
                    width: 40.0,
                    height: 20.0,
                }],
                ..Default::default()
            })));
            let container = GameContainer::new(&overlay, router.clone()).unwrap();
            container.set_game(Some(&game));
            assert_eq!(
                container.hitTest(NSPoint::new(20.0, 220.0)).as_deref(),
                Some(&*overlay)
            );
            assert_eq!(
                container.hitTest(NSPoint::new(200.0, 40.0)).as_deref(),
                Some(game.as_ref() as &NSView)
            );
            let mouse = |kind, x, y| {
                NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                kind, NSPoint::new(x, y), NSEventModifierFlags::empty(), 0.0,
                native.windowNumber(), None, 0, 1, 1.0).unwrap()
            };
            let app = NSApplication::sharedApplication(mtm);
            dispatch(&mut window);
            app.postEvent_atStart(&mouse(NSEventType::LeftMouseDown, 20.0, 220.0), false);
            assert!(inputs(dispatch(&mut window)).is_empty());
            assert_eq!(router.lock().unwrap().captured(0), Some(Target::Overlay));
            router.lock().unwrap().set_input(OverlayInput::default());
            assert_eq!(
                container.hitTest(NSPoint::new(200.0, 40.0)).as_deref(),
                Some(&*overlay)
            );
            app.postEvent_atStart(&mouse(NSEventType::LeftMouseDragged, 200.0, 40.0), false);
            app.postEvent_atStart(&mouse(NSEventType::LeftMouseUp, 200.0, 40.0), false);
            assert!(inputs(dispatch(&mut window)).is_empty());
            assert_eq!(router.lock().unwrap().captured(0), None);
            app.postEvent_atStart(&mouse(NSEventType::LeftMouseDown, 200.0, 40.0), false);
            dispatch(&mut window);
            assert_eq!(router.lock().unwrap().captured(0), Some(Target::Game));
            // Hidden windows do not dispatch canvas events; deliver to the
            // actual hit-test result as in the physical input checks above.
            let receiver = container.hitTest(NSPoint::new(200.0, 40.0)).unwrap();
            receiver.mouseDown(&mouse(NSEventType::LeftMouseDown, 200.0, 40.0));
            assert!(
                inputs(dispatch(&mut window))
                    .iter()
                    .any(|event| event.0 == 0x201)
            );
            router.lock().unwrap().set_input(OverlayInput::exclusive());
            assert_eq!(
                container.hitTest(NSPoint::new(20.0, 220.0)).as_deref(),
                Some(game.as_ref() as &NSView)
            );
            app.postEvent_atStart(&mouse(NSEventType::LeftMouseDragged, 20.0, 220.0), false);
            app.postEvent_atStart(&mouse(NSEventType::LeftMouseUp, 20.0, 220.0), false);
            dispatch(&mut window);
            receiver.mouseDragged(&mouse(NSEventType::LeftMouseDragged, 20.0, 220.0));
            receiver.mouseUp(&mouse(NSEventType::LeftMouseUp, 20.0, 220.0));
            let events = inputs(dispatch(&mut window));
            assert!(events.iter().any(|event| event.0 == 0x200));
            assert!(events.iter().any(|event| event.0 == 0x202));
            assert_eq!(router.lock().unwrap().captured(0), None);
            container.set_game(None);
            let library_frame = native.frame();
            for (width, height) in [(1600, 900), (800, 600)] {
                let embedded = GameView::new(width, height).unwrap();
                container.set_game(Some(&embedded));
                let size = embedded.bounds().size;
                assert!(
                    (size.width / size.height - f64::from(width) / f64::from(height)).abs() < 0.001
                );
                assert_eq!(embedded.bounds().size, container.bounds().size);
                assert_eq!(overlay.bounds().size, container.bounds().size);
                container.set_game(None);
                assert_eq!(native.frame(), library_frame);
            }
            native.setContentView(Some(&game));
            native.makeFirstResponder(Some(&game));
            dispatch(&mut window);
        }
        println!("ok container hitTest, logical coordinates and mouse capture");

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
        dispatch(&mut window);
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
        dispatch(&mut window);
        post(
            mtm,
            &key_event(
                &native,
                NSEventType::KeyDown,
                0x00,
                NSEventModifierFlags::empty(),
            ),
        );
        assert!(inputs(dispatch(&mut window)).is_empty());
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
        assert_eq!(inputs(dispatch(&mut window)).len(), 1);
        println!("ok dialogs");

        // Exercise delegate notifications without showing a window, sleeping
        // the machine, or terminating the test process.
        let delegate = native.delegate().unwrap();
        let note = unsafe {
            NSNotification::notificationWithName_object(
                &NSString::from_str("test lifecycle"),
                Some(&native),
            )
        };
        dispatch(&mut window);
        let _: () = unsafe { msg_send![&*delegate, windowDidMiniaturize: &*note] };
        assert!(inputs(dispatch(&mut window)).contains(&(5, 1, 0)));
        assert_eq!(window.native_client_size().unwrap(), (0, 0));
        let _: () = unsafe { msg_send![&*delegate, windowDidDeminiaturize: &*note] };
        assert!(
            inputs(dispatch(&mut window))
                .iter()
                .any(|&(message, state, _)| message == 5 && state == 0)
        );
        assert_ne!(window.native_client_size().unwrap(), (0, 0));
        let _: () = unsafe { msg_send![&*delegate, windowDidChangeOcclusionState: &*note] };
        assert_eq!(window.native_client_size().unwrap(), (0, 0));
        println!("ok minimize, restore and occlusion");

        let app = NSApplication::sharedApplication(mtm);
        let application = app.delegate().unwrap();
        let _: () = unsafe { msg_send![&*application, applicationDidHide: &*note] };
        assert!(inputs(dispatch(&mut window)).contains(&(0x1c, 0, 0)));
        assert_eq!(window.native_client_size().unwrap(), (0, 0));
        let _: () = unsafe { msg_send![&*application, applicationDidUnhide: &*note] };
        let _: () = unsafe { msg_send![&*application, applicationDidBecomeActive: &*note] };
        assert!(inputs(dispatch(&mut window)).contains(&(0x1c, 1, 0)));
        let _: () = unsafe { msg_send![&*application, applicationDidResignActive: &*note] };
        assert!(inputs(dispatch(&mut window)).contains(&(0x1c, 0, 0)));
        println!("ok hide and activation");

        let center = objc2_app_kit::NSWorkspace::sharedWorkspace().notificationCenter();
        unsafe {
            center
                .postNotificationName_object(objc2_app_kit::NSWorkspaceWillSleepNotification, None);
        }
        assert!(
            dispatch(&mut window)
                .iter()
                .any(|e| matches!(e, Event::AudioSuspended(true)))
        );
        assert_eq!(window.native_client_size().unwrap(), (0, 0));
        unsafe {
            center.postNotificationName_object(objc2_app_kit::NSWorkspaceDidWakeNotification, None);
        }
        assert!(
            dispatch(&mut window)
                .iter()
                .any(|e| matches!(e, Event::AudioSuspended(false)))
        );
        assert_ne!(window.native_client_size().unwrap(), (0, 0));
        println!("ok synthetic sleep and wake");

        let reply: objc2_app_kit::NSApplicationTerminateReply =
            unsafe { msg_send![&*application, applicationShouldTerminate: &*app] };
        assert_eq!(
            reply,
            objc2_app_kit::NSApplicationTerminateReply::TerminateLater
        );
        assert!(
            dispatch(&mut window)
                .iter()
                .any(|e| matches!(e, Event::Close))
        );
        native.performClose(None);
        assert!(
            dispatch(&mut window)
                .iter()
                .any(|e| matches!(e, Event::Close))
        );
        // A second Cmd+Q is the explicit escape from an unresponsive guest.
        let quit = objc2_app_kit::NSMenuItem::new(mtm);
        let _: () = unsafe { msg_send![&*application, hostQuit: &*quit] };
        assert!(
            dispatch(&mut window)
                .iter()
                .any(|e| matches!(e, Event::ForceClose))
        );
        unsafe {
            center.postNotificationName_object(
                objc2_app_kit::NSWorkspaceWillPowerOffNotification,
                None,
            );
        }
        let ending = inputs(dispatch(&mut window));
        assert_eq!(ending, vec![(0x11, 0, 0), (0x16, 1, 0)]);
        println!("ok logout and shutdown notifications");
        println!("ok close and application quit");
        drop(window);
        assert!(app.delegate().is_none());
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/scratch/game-view/run-loop-test");
        std::fs::create_dir_all(&directory).unwrap();
        let called = Rc::new(RefCell::new(0));
        let completed = called.clone();
        let view = adv_shell::window::GameView::new(320, 240).unwrap();
        let session = adv_shell::Session::new(view, adv_shell::Options::new(directory.join("missing.exe"), directory.join("data")), Box::new(move |result| {
            assert!(MainThreadMarker::new().is_some());
            assert!(result.is_err());
            *completed.borrow_mut() += 1;
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            app.stop(None);
            let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                NSEventType::ApplicationDefined, NSPoint::new(0.0, 0.0), NSEventModifierFlags::empty(), 0.0, 0, None, 0, 0, 0).unwrap();
            app.postEvent_atStart(&event, false);
        })).unwrap();
        app.run();
        assert_eq!(*called.borrow(), 1);
        drop(session);
        assert_eq!(*called.borrow(), 1);
        println!("ok Session driven by external NSApp.run and one-shot main-thread completion");
        println!("ok all");
    }
}
