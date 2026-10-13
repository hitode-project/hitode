//! The player frontend: a Tauri window whose webview draws the game library
//! and, while a game runs, a transparent in-game overlay above the native game
//! view that adv-shell presents into (decision 25).
pub mod overlay;

use serde::Serialize;
#[cfg(target_os = "macos")]
mod game;
#[cfg(target_os = "macos")]
use game::{post_text, start_game};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostInfo {
    platform: &'static str,
    /// A native game view is attached under the webview.
    game_view: bool,
}

#[tauri::command]
fn host_info() -> HostInfo {
    HostInfo {
        platform: std::env::consts::OS,
        game_view: cfg!(target_os = "macos"),
    }
}

/// Sends text to the running guest as IME characters (Runtime::post_text).
#[cfg(not(target_os = "macos"))]
#[tauri::command]
fn post_text(text: String) -> Result<usize, String> {
    let _ = text;
    Err("no game is running".into())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().manage(overlay::OverlayState::default());
    #[cfg(target_os = "macos")]
    let builder = builder
        .setup(|app| {
            game::setup(app)?;
            Ok(())
        })
        .on_window_event(game::window_event)
        .invoke_handler(tauri::generate_handler![
            host_info,
            post_text,
            start_game,
            overlay::set_overlay_input
        ]);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        host_info,
        post_text,
        overlay::set_overlay_input
    ]);
    let app = builder
        .build(tauri::generate_context!())
        .expect("the launcher failed to start");
    #[cfg(target_os = "macos")]
    app.run(game::run_event);
    #[cfg(not(target_os = "macos"))]
    app.run(|_, _| {});
}
