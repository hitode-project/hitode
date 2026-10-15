//! The pause command in the application menu.
//!
//! The game keeps every key it would get on Windows, Esc included: many games
//! use it for their own system menu, cancel or skip. Command-key shortcuts
//! never reach the guest, so the overlay's pause menu is opened by ⌘. from
//! the macOS application menu, which emits a `pause` event to the webview.
//! Windows and Linux have no menu bar yet; the overlay's pause button stays.
//!
//! The predefined Quit item sends `terminate:`, which tao's delegate does not
//! intercept, so the process would exit without letting a running game save.
//! It is replaced by an item that calls AppHandle::exit. On macOS, while a
//! game runs, the item ends only the game, as the pause menu's 退出游戏 does;
//! the Apple quit event (logout, shut down) still defers the exit until the
//! game has closed.
use tauri::menu::{Menu, MenuItem, MenuItemKind, PredefinedMenuItem};
use tauri::{AppHandle, Manager, Runtime};

const PAUSE: &str = "pause";
const QUIT: &str = "quit";

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
        if let Some(MenuItemKind::Predefined(quit)) = app_menu.items()?.last() {
            let text = quit.text()?;
            app_menu.remove(quit)?;
            app_menu.append(&MenuItem::with_id(app, QUIT, text, true, Some("Cmd+Q"))?)?;
        }
    }
    app.manage(PauseItem(pause));
    Ok(menu)
}

pub fn on_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        PAUSE => {
            #[cfg(target_os = "macos")]
            crate::game::pause(app);
            #[cfg(not(target_os = "macos"))]
            let _ = tauri::Emitter::emit(app, PAUSE, ());
        }
        // While a game runs, Quit ends only the game; the next one quits.
        #[cfg(target_os = "macos")]
        QUIT => {
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                if !crate::game::end_game() {
                    handle.exit(0);
                }
            });
        }
        #[cfg(not(target_os = "macos"))]
        QUIT => app.exit(0),
        _ => {}
    }
}

#[tauri::command]
pub fn set_pause_available<R: Runtime>(app: AppHandle<R>, available: bool) -> Result<(), String> {
    match app.try_state::<PauseItem<R>>() {
        Some(item) => item.0.set_enabled(available).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

#[tauri::command]
pub fn set_menu_language<R: Runtime>(app: AppHandle<R>, language: String) -> Result<(), String> {
    if let Some(item) = app.try_state::<PauseItem<R>>() {
        item.0
            .set_text(match language.as_str() {
                "zh" => "暂停",
                "ja" => "一時停止",
                _ => "Pause",
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
