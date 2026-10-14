//! The player frontend: a Tauri window whose webview draws the game library
//! and, while a game runs, a transparent in-game overlay above the native game
//! view that adv-shell presents into (decision 25).
#[cfg(desktop)]
mod library;
#[cfg(desktop)]
mod menu;
pub mod overlay;
mod report;

use serde::Serialize;
#[cfg(target_os = "macos")]
mod chrome;
#[cfg(target_os = "macos")]
mod game;
#[cfg(target_os = "macos")]
use game::{post_text, start_game, stop_game};

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
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            use tauri::Manager;
            app.manage(std::sync::Mutex::new(library::Library::load(
                app.path().app_data_dir()?,
            )?));
            let menu = menu::build(app.handle())?;
            if cfg!(target_os = "macos") {
                app.set_menu(menu)?;
            }
            #[cfg(target_os = "macos")]
            game::setup(app)?;
            Ok(())
        })
        .on_menu_event(|app, event| menu::on_event(app, event.id().as_ref()));
    #[cfg(target_os = "macos")]
    let builder =
        builder
            .on_window_event(game::window_event)
            .invoke_handler(tauri::generate_handler![
                host_info,
                post_text,
                start_game,
                game::pause_game,
                game::set_game_filter,
                stop_game,
                overlay::set_overlay_input,
                report::report_base,
                report::save_report,
                menu::set_pause_available,
                chrome::set_title_bar,
                library::library_pick_folder,
                library::library_list,
                library::library_inspect,
                library::library_scan,
                library::library_add,
                library::library_update,
                library::library_remove,
                library::library_restore,
                library::reveal_path
            ]);
    #[cfg(all(desktop, not(target_os = "macos")))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        host_info,
        post_text,
        overlay::set_overlay_input,
        report::report_base,
        report::save_report,
        menu::set_pause_available,
        library::library_pick_folder,
        library::library_list,
        library::library_inspect,
        library::library_scan,
        library::library_add,
        library::library_update,
        library::library_remove,
        library::library_restore,
        library::reveal_path
    ]);
    #[cfg(mobile)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        host_info,
        post_text,
        overlay::set_overlay_input,
        report::report_base,
        report::save_report
    ]);
    let app = builder
        .build(tauri::generate_context!())
        .expect("the launcher failed to start");
    #[cfg(target_os = "macos")]
    app.run(game::run_event);
    #[cfg(not(target_os = "macos"))]
    app.run(|_, _| {});
}
