//! The player frontend: a Tauri window whose webview draws the game library
//! and, while a game runs, a transparent in-game overlay above the native game
//! view that adv-shell presents into (decision 25).
pub mod overlay;

use serde::Serialize;

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
        game_view: false,
    }
}

/// Sends text to the running guest as IME characters (Runtime::post_text).
#[tauri::command]
fn post_text(text: String) -> Result<usize, String> {
    let _ = text;
    Err("no game is running".into())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(overlay::OverlayState::default())
        .invoke_handler(tauri::generate_handler![
            host_info,
            post_text,
            overlay::set_overlay_input
        ])
        .run(tauri::generate_context!())
        .expect("the launcher failed to start");
}
