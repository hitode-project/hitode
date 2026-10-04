//! UI-owned native menus built only from immutable presentation data.
use super::*;
use adv_runtime::{MenuBar, MenuCommand, MenuEvent, MenuItem};
use std::collections::BTreeMap;

struct OwnedMenu(HMENU);
impl Drop for OwnedMenu {
    fn drop(&mut self) {
        // SAFETY: the handle is either private or has been detached from HWND.
        unsafe {
            DestroyMenu(self.0);
        }
    }
}
pub(super) struct HostMenu {
    handle: OwnedMenu,
    window: u32,
    root: u32,
    commands: BTreeMap<u32, MenuCommand>,
    popups: BTreeMap<usize, (u32, u16)>,
}
fn failure(operation: &str) -> String {
    format!("{operation} failed: {}", unsafe { GetLastError() })
}
impl HostMenu {
    pub(super) fn build(bar: &MenuBar) -> Result<Self, String> {
        fn build(
            items: &[MenuItem],
            popup: bool,
            enabled: bool,
            commands: &mut BTreeMap<u32, MenuCommand>,
            popups: &mut BTreeMap<usize, (u32, u16)>,
        ) -> Result<OwnedMenu, String> {
            let handle = unsafe {
                if popup {
                    CreatePopupMenu()
                } else {
                    CreateMenu()
                }
            };
            if handle.is_null() {
                return Err(failure("CreateMenu"));
            }
            let menu = OwnedMenu(handle);
            for (position, item) in items.iter().enumerate() {
                let selectable = enabled && item.supported && item.flags & 3 == 0;
                let child = if item.submenu == 0 {
                    None
                } else {
                    Some(build(&item.children, true, selectable, commands, popups)?)
                };
                let child_handle = child.as_ref().map_or(null_mut(), |child| child.0);
                let id = if item.submenu == 0 && item.flags & 0x800 == 0 {
                    let id = commands.len() as u32 + 1;
                    if id > 16384 {
                        return Err("native menu exceeds command capacity".into());
                    }
                    commands.insert(id, item.command);
                    id
                } else {
                    0
                };
                let mut text = if item.supported {
                    item.text.clone()
                } else {
                    "Unavailable menu item".encode_utf16().collect()
                };
                text.push(0);
                let mut info = MENUITEMINFOW {
                    cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_SUBMENU,
                    fType: item.flags
                        & (MFT_SEPARATOR
                            | MFT_RADIOCHECK
                            | MFT_MENUBREAK
                            | MFT_MENUBARBREAK
                            | MFT_RIGHTJUSTIFY
                            | MFT_RIGHTORDER),
                    fState: (item.flags & (MFS_CHECKED | MFS_DEFAULT))
                        | if selectable && item.flags & 0x800 == 0 {
                            0
                        } else {
                            MFS_DISABLED
                        },
                    wID: id,
                    hSubMenu: child_handle,
                    ..unsafe { std::mem::zeroed() }
                };
                if item.flags & 0x800 == 0 {
                    info.fMask |= MIIM_STRING;
                    info.dwTypeData = text.as_mut_ptr();
                    info.cch = (text.len() - 1) as u32;
                }
                if unsafe { InsertMenuItemW(menu.0, position as u32, 1, &info) } == 0 {
                    return Err(failure("InsertMenuItemW"));
                }
                if let Some(child) = child {
                    popups.insert(child.0 as usize, (item.submenu, position as u16));
                    // Insertion transfers child ownership to its parent.
                    std::mem::forget(child);
                }
            }
            Ok(menu)
        }
        let mut commands = BTreeMap::new();
        let mut popups = BTreeMap::new();
        let handle = build(
            &bar.items,
            false,
            bar.key.enabled,
            &mut commands,
            &mut popups,
        )?;
        Ok(Self {
            handle,
            window: bar.key.window,
            root: bar.key.menu,
            commands,
            popups,
        })
    }
    pub(super) fn event(&self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<MenuEvent> {
        match message {
            WM_COMMAND if lparam == 0 && wparam >> 16 == 0 => self
                .commands
                .get(&(wparam as u32))
                .copied()
                .map(MenuEvent::Command),
            WM_INITMENU if wparam == self.handle.0 as usize => Some(MenuEvent::Initialize {
                window: self.window,
                root: self.root,
                menu: self.root,
                position: None,
            }),
            WM_INITMENUPOPUP if (lparam as usize >> 16) & 0xffff == 0 => self
                .popups
                .get(&wparam)
                .map(|&(menu, position)| MenuEvent::Initialize {
                    window: self.window,
                    root: self.root,
                    menu,
                    position: Some(position),
                }),
            _ => None,
        }
    }
}
impl Window {
    pub fn set_menu_event_sink(&mut self, sink: MenuSink) {
        self.state.menu_sink = Some(sink);
    }
    pub fn set_menu_bar(&mut self, bar: &MenuBar) -> Result<(), String> {
        let next = if bar.key.menu == 0 || bar.items.is_empty() {
            None
        } else {
            Some(HostMenu::build(bar)?)
        };
        let handle = next.as_ref().map_or(null_mut(), |menu| menu.handle.0);
        let mut client: RECT = unsafe { std::mem::zeroed() };
        if unsafe { GetClientRect(self.hwnd, &mut client) } == 0 {
            return Err(failure("GetClientRect"));
        }
        if unsafe { SetMenu(self.hwnd, handle) } == 0 {
            return Err(failure("SetMenu"));
        }
        // Old handles are now detached; replacing the model retires the whole tree.
        self.state.menu = next;
        if unsafe { DrawMenuBar(self.hwnd) } == 0 {
            return Err(failure("DrawMenuBar"));
        }
        set_client_size(
            self.hwnd,
            (client.right - client.left).max(1) as u32,
            (client.bottom - client.top).max(1) as u32,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_menu_preserves_client_size_states_and_specific_guest_target() {
        let mut window = Window::new("authored host menu", 320, 180, true).unwrap();
        let key = adv_runtime::MenuKey {
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
        disabled.command.position = 1;
        disabled.command.id = 77;
        let bar = MenuBar {
            key,
            items: vec![MenuItem {
                text: "&設定".encode_utf16().collect(),
                flags: 0,
                supported: true,
                command: MenuCommand::default(),
                submenu: command.menu,
                children: vec![leaf, disabled],
            }],
        };
        let client = |window: &Window| {
            let mut r: RECT = unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { GetClientRect(window.hwnd, &mut r) }, 0);
            (r.right, r.bottom)
        };
        assert_eq!(client(&window), (320, 180));
        window.set_menu_bar(&bar).unwrap();
        assert_eq!(client(&window), (320, 180));
        let root = unsafe { GetMenu(window.hwnd) };
        assert!(!root.is_null());
        let popup = unsafe { GetSubMenu(root, 0) };
        assert!(!popup.is_null());
        let mut text = [0u16; 64];
        let length =
            unsafe { GetMenuStringW(root, 0, text.as_mut_ptr(), text.len() as i32, MF_BYPOSITION) };
        assert_eq!(
            String::from_utf16(&text[..length as usize]).unwrap(),
            "&設定"
        );
        assert_ne!(
            unsafe { GetMenuState(popup, 0, MF_BYPOSITION) } & MF_CHECKED,
            0
        );
        assert_ne!(
            unsafe { GetMenuState(popup, 1, MF_BYPOSITION) } & MF_DISABLED,
            0
        );
        let native_id = unsafe { GetMenuItemID(popup, 0) };
        unsafe {
            SendMessageW(window.hwnd, WM_COMMAND, native_id as usize, 0);
        }
        assert!(window.pump().into_iter().any(
            |event| matches!(event,Event::Menu(MenuEvent::Command(actual)) if actual==command)
        ));
        // Init callbacks deliver immediately even inside native menu dispatch;
        // there is no wait for the outer UI loop or guest execution.
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = events.clone();
        window.set_menu_event_sink(Box::new(move |event| {
            sink.borrow_mut().push(event);
            Ok(())
        }));
        unsafe {
            SendMessageW(window.hwnd, WM_INITMENUPOPUP, popup as usize, 0);
        }
        assert_eq!(
            *events.borrow(),
            [MenuEvent::Initialize {
                window: key.window,
                root: key.menu,
                menu: command.menu,
                position: Some(0)
            }]
        );
        window.set_menu_bar(&MenuBar::default()).unwrap();
        assert!(unsafe { GetMenu(window.hwnd) }.is_null());
        assert_eq!(client(&window), (320, 180));
    }
}
