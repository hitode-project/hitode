//! Guest menus in the macOS menu bar, after the application menu, and guest
//! popups as tracked NSMenus. Built only from immutable presentation data.
use super::*;
use adv_runtime::{MenuBar, MenuCommand, MenuEvent, MenuItem, MenuKey, PopupPresentation};
use objc2::sel;
use objc2_foundation::NSRunLoopCommonModes;
use std::sync::atomic::{AtomicBool, Ordering};

/// The guest caption without its mnemonic markers and accelerator column.
pub(super) fn title(text: &[u16]) -> String {
    let text = String::from_utf16_lossy(text);
    let caption = text.split('\t').next().unwrap_or_default();
    let mut title = String::with_capacity(caption.len());
    let mut chars = caption.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if chars.peek() == Some(&'&') {
                title.push('&');
                chars.next();
            }
        } else {
            title.push(c);
        }
    }
    title
}

struct Popup {
    menu: Retained<NSMenu>,
    active: Arc<AtomicBool>,
    choice: Option<isize>,
}

/// Menu state the callbacks read and update.
#[derive(Default)]
pub(in crate::window) struct Runtime {
    key: MenuKey,
    commands: BTreeMap<isize, MenuCommand>,
    /// Native submenus of the bar and popup, as guest (menu, position).
    submenus: BTreeMap<usize, (u32, u16)>,
    open: u32,
    popup: Option<Popup>,
}
impl Runtime {
    pub(super) fn opened(&mut self, menu: &NSMenu) -> Option<Vec<MenuEvent>> {
        let at = menu as *const NSMenu as usize;
        let tracked = self
            .popup
            .as_ref()
            .is_some_and(|p| Retained::as_ptr(&p.menu) as usize == at);
        let submenu = self.submenus.get(&at).copied();
        if submenu.is_none() && !tracked {
            return None;
        }
        let mut events = Vec::new();
        // The bar initializes once per tracking session, before its submenu.
        if self.popup.is_none() && self.open == 0 {
            events.push(MenuEvent::Initialize {
                window: self.key.window,
                root: self.key.menu,
                menu: self.key.menu,
                position: None,
            });
        }
        self.open += 1;
        if let Some((menu, position)) = submenu {
            events.push(MenuEvent::Initialize {
                window: self.key.window,
                root: self.key.menu,
                menu,
                position: Some(position),
            });
        }
        Some(events)
    }
    pub(super) fn closed(&mut self, menu: &NSMenu) {
        let at = menu as *const NSMenu as usize;
        let tracked = self
            .popup
            .as_ref()
            .is_some_and(|p| Retained::as_ptr(&p.menu) as usize == at);
        if self.submenus.contains_key(&at) || tracked {
            self.open = self.open.saturating_sub(1);
        }
    }
    pub(super) fn command(&mut self, tag: isize) -> Option<MenuEvent> {
        if let Some(popup) = &mut self.popup {
            popup.choice = Some(tag);
            return None;
        }
        self.commands.get(&tag).copied().map(MenuEvent::Command)
    }
    pub(super) fn poll(&self) {
        if let Some(popup) = &self.popup
            && !popup.active.load(Ordering::Acquire)
        {
            popup.menu.cancelTracking();
        }
    }
}

/// The guest's top-level items in the menu bar, after the application menu.
pub(super) struct Menus {
    main: Retained<NSMenu>,
    guest: Vec<Retained<NSMenuItem>>,
}

fn item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    key: &str,
) -> Retained<NSMenuItem> {
    // SAFETY: the action is a selector the target or responder chain implements.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    }
}

fn menu(mtm: MainThreadMarker, title: &str) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    // Guest state decides every item's enablement.
    menu.setAutoenablesItems(false);
    menu
}

impl Menus {
    /// Builds the CLI window's application menu.
    pub(super) fn new(mtm: MainThreadMarker, app: &NSApplication, target: &Target) -> Self {
        let main = menu(mtm, "");
        let application = item(mtm, "ADV", None, "");
        let submenu = menu(mtm, "ADV");
        submenu.setAutoenablesItems(true);
        submenu.addItem(&item(mtm, "Hide ADV", Some(sel!(hide:)), "h"));
        let quit = item(mtm, "Quit", Some(sel!(hostQuit:)), "q");
        // SAFETY: the target outlives the menu; both live as long as `Window`.
        unsafe { quit.setTarget(Some(target)) };
        submenu.addItem(&item(mtm, "Minimize", Some(sel!(performMiniaturize:)), "m"));
        let full = item(mtm, "Enter Full Screen", Some(sel!(toggleFullScreen:)), "f");
        full.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Control,
        );
        submenu.addItem(&full);
        submenu.addItem(&NSMenuItem::separatorItem(mtm));
        submenu.addItem(&quit);
        application.setSubmenu(Some(&submenu));
        main.addItem(&application);
        app.setMainMenu(Some(&main));
        Self {
            main,
            guest: Vec::new(),
        }
    }
    /// Uses the embedding host's main menu, which keeps its own items.
    pub(super) fn attach(mtm: MainThreadMarker, app: &NSApplication) -> Self {
        let main = app.mainMenu().unwrap_or_else(|| {
            let main = menu(mtm, "");
            app.setMainMenu(Some(&main));
            main
        });
        Self {
            main,
            guest: Vec::new(),
        }
    }
    fn add(&mut self, item: Retained<NSMenuItem>) {
        let at = (self.guest.len() + 1).min(self.main.numberOfItems() as usize);
        self.main.insertItem_atIndex(&item, at as isize);
        self.guest.push(item);
    }
    pub(super) fn clear(&mut self) {
        for item in self.guest.drain(..) {
            // The host may have replaced its main menu meanwhile.
            if self.main.indexOfItem(&item) >= 0 {
                self.main.removeItem(&item);
            }
        }
    }
}

/// Builds `items` into `menu`, registering commands and submenus.
fn build(
    mtm: MainThreadMarker,
    target: &Target,
    runtime: &mut Runtime,
    menu: &NSMenu,
    items: &[MenuItem],
    enabled: bool,
) -> Result<(), String> {
    for (position, source) in items.iter().enumerate() {
        if source.flags & 0x800 != 0 {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            continue;
        }
        let selectable = enabled && source.supported && source.flags & 3 == 0;
        let caption = if source.supported {
            title(&source.text)
        } else {
            "Unavailable menu item".into()
        };
        let native = item(mtm, &caption, None, "");
        native.setEnabled(selectable);
        if source.flags & 8 != 0 {
            native.setState(objc2_app_kit::NSControlStateValueOn);
        }
        if source.submenu == 0 {
            let tag = runtime.commands.len() as isize + 1;
            if tag > 16384 {
                return Err("native menu exceeds command capacity".into());
            }
            runtime.commands.insert(tag, source.command);
            native.setTag(tag);
            // SAFETY: the target implements the action and outlives the item.
            unsafe {
                native.setAction(Some(sel!(guestMenuCommand:)));
                native.setTarget(Some(target));
            }
        } else {
            let child = self::menu(mtm, &caption);
            child.setDelegate(Some(ProtocolObject::from_ref(target)));
            runtime.submenus.insert(
                Retained::as_ptr(&child) as usize,
                (source.submenu, position as u16),
            );
            build(mtm, target, runtime, &child, &source.children, selectable)?;
            native.setSubmenu(Some(&child));
        }
        menu.addItem(&native);
    }
    Ok(())
}

impl Projection {
    pub(crate) fn set_menu_event_sink(&mut self, sink: MenuSink) {
        *self.hub.menu_sink.borrow_mut() = Some(sink);
    }

    pub(crate) fn set_menu_bar(&mut self, bar: &MenuBar) -> Result<(), String> {
        let present = bar.key.menu != 0 && !bar.items.is_empty();
        if let Some(popup) = &bar.popup {
            // A transient popup does not own or detach the window's menu bar.
            let native = menu(self.mtm, "");
            native.setDelegate(Some(ProtocolObject::from_ref(&*self.target)));
            let mut runtime = std::mem::take(&mut *self.hub.menus.borrow_mut());
            let saved = (
                runtime.key,
                runtime.commands.clone(),
                runtime.submenus.clone(),
            );
            runtime.commands.clear();
            runtime.submenus.clear();
            runtime.key = bar.key;
            let built = if present {
                build(
                    self.mtm,
                    &self.target,
                    &mut runtime,
                    &native,
                    &bar.items,
                    bar.key.enabled,
                )
            } else {
                Ok(())
            };
            runtime.popup = Some(Popup {
                menu: native,
                active: popup.active.clone(),
                choice: None,
            });
            *self.hub.menus.borrow_mut() = runtime;
            let tracked = built.and_then(|()| {
                if present && !self.hidden && popup.active.load(Ordering::Acquire) {
                    self.track_popup(bar, popup)
                } else {
                    Ok(())
                }
            });
            // Restore the bar's commands for later bar choices.
            let mut runtime = self.hub.menus.borrow_mut();
            runtime.popup = None;
            (runtime.key, runtime.commands, runtime.submenus) = saved;
            runtime.open = 0;
            return tracked;
        }
        self.menus.clear();
        let mut runtime = Runtime {
            key: bar.key,
            ..Runtime::default()
        };
        if present {
            for (position, source) in bar.items.iter().enumerate() {
                let caption = title(&source.text);
                let top = item(self.mtm, &caption, None, "");
                let child = menu(self.mtm, &caption);
                child.setDelegate(Some(ProtocolObject::from_ref(&*self.target)));
                if source.submenu == 0 {
                    // AppKit's bar holds only menus; a bare command opens one.
                    build(
                        self.mtm,
                        &self.target,
                        &mut runtime,
                        &child,
                        std::slice::from_ref(source),
                        bar.key.enabled,
                    )?;
                } else {
                    runtime.submenus.insert(
                        Retained::as_ptr(&child) as usize,
                        (source.submenu, position as u16),
                    );
                    let selectable = bar.key.enabled && source.supported && source.flags & 3 == 0;
                    build(
                        self.mtm,
                        &self.target,
                        &mut runtime,
                        &child,
                        &source.children,
                        selectable,
                    )?;
                }
                top.setSubmenu(Some(&child));
                top.setEnabled(bar.key.enabled && source.supported && source.flags & 3 == 0);
                self.menus.add(top);
            }
        }
        *self.hub.menus.borrow_mut() = runtime;
        if bar.key.window != 0 {
            self.dialogs.borrow_mut().set_main_enabled(bar.key.enabled);
            self.hub
                .main_enabled
                .set(self.dialogs.borrow().main_enabled());
        }
        Ok(())
    }

    fn track_popup(&mut self, bar: &MenuBar, popup: &PopupPresentation) -> Result<(), String> {
        let (width, height) = self.hub.canvas.get();
        let client = self.view.client();
        let viewport = Viewport::fit(client, width, height);
        let x = viewport.x + f64::from(popup.x) * viewport.width / f64::from(width.max(1));
        let y = viewport.y + f64::from(popup.y) * viewport.height / f64::from(height.max(1));
        let native = self
            .hub
            .menus
            .borrow()
            .popup
            .as_ref()
            .map(|popup| popup.menu.clone())
            .ok_or("popup menu missing")?;
        // The worker may end the popup; poll its token during tracking.
        // SAFETY: the target implements popupPoll: and outlives the timer.
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                0.03,
                &self.target,
                sel!(popupPoll:),
                None,
                true,
            )
        };
        // SAFETY: the timer is invalidated before this returns.
        unsafe {
            objc2_foundation::NSRunLoop::currentRunLoop()
                .addTimer_forMode(&timer, NSRunLoopCommonModes)
        };
        native.popUpMenuPositioningItem_atLocation_inView(
            None,
            NSPoint::new(x, client.1 - y),
            Some(&self.view),
        );
        timer.invalidate();
        if !popup.active.load(Ordering::Acquire) {
            return Ok(());
        }
        let event = {
            let runtime = self.hub.menus.borrow();
            runtime
                .popup
                .as_ref()
                .and_then(|p| p.choice)
                .and_then(|tag| runtime.commands.get(&tag).copied())
        }
        .map_or(
            MenuEvent::Cancel {
                window: bar.key.window,
                root: bar.key.menu,
            },
            MenuEvent::Command,
        );
        self.hub.menu_event(event);
        if matches!(event, MenuEvent::Command(_)) {
            // A rejected choice must still retire the guest's tracking.
            self.hub.menu_event(MenuEvent::Cancel {
                window: bar.key.window,
                root: bar.key.menu,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_drop_mnemonics_and_the_accelerator_column() {
        let wide = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(title(&wide("&設定")), "設定");
        assert_eq!(title(&wide("オートプレイ\tF6")), "オートプレイ");
        assert_eq!(title(&wide("Save && &Quit")), "Save & Quit");
        assert_eq!(title(&wide("")), "");
    }
}
