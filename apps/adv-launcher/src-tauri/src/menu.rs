//! The pause command in the application menu.
//!
//! The game keeps every key it would get on Windows, Esc included: many games
//! use it for their own system menu, cancel or skip. Command-key shortcuts
//! never reach the guest, so the overlay's pause menu is opened by ⌘. from
//! the macOS application menu, which emits a `pause` event to the webview.
//! Windows and Linux have no menu bar yet; the overlay's pause button stays.
use tauri::menu::{Menu, MenuItem, MenuItemKind, PredefinedMenuItem};
use tauri::{AppHandle, Emitter, Manager, Runtime};

const PAUSE: &str = "pause";

/// The pause item, enabled only while the game screen is shown.
pub struct PauseItem<R: Runtime>(MenuItem<R>);

/// Builds the default menu with 暂停 (⌘.) after About in the application menu.
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let menu = Menu::default(app)?;
    let pause = MenuItem::with_id(app, PAUSE, "暂停", false, Some("Cmd+Period"))?;
    if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
        // About, separator, then the pause item in its own group.
        app_menu.insert(&pause, 2)?;
        app_menu.insert(&PredefinedMenuItem::separator(app)?, 3)?;
    }
    app.manage(PauseItem(pause));
    Ok(menu)
}

pub fn on_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if id == PAUSE {
        let _ = app.emit(PAUSE, ());
    }
}

#[tauri::command]
pub fn set_pause_available<R: Runtime>(app: AppHandle<R>, available: bool) -> Result<(), String> {
    match app.try_state::<PauseItem<R>>() {
        Some(item) => item.0.set_enabled(available).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}
