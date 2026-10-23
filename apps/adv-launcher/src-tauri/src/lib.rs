//! The player frontend: a Tauri window whose webview draws the game library
//! and, while a game runs, a transparent in-game overlay above the native game
//! view that adv-shell presents into.
#[cfg(target_os = "android")]
pub mod android;
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
mod drive;
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
#[cfg_attr(target_os = "android", path = "game_android.rs")]
#[cfg_attr(target_os = "ios", path = "game_ios.rs")]
mod game;
#[cfg(target_os = "ios")]
mod jit_ios;
mod library;
#[cfg(desktop)]
mod menu;
#[cfg(mobile)]
mod mobile_view;
pub mod overlay;
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
mod play;
mod report;
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
mod settings;
mod update;
mod vndb;

use serde::Serialize;
#[cfg(target_os = "macos")]
mod chrome;
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
use game::{post_text, start_game, stop_game};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostInfo {
    platform: &'static str,
    version: &'static str,
    /// A native game view is attached under the webview.
    game_view: bool,
}

#[tauri::command]
fn host_info() -> HostInfo {
    HostInfo {
        platform: std::env::consts::OS,
        version: env!("CARGO_PKG_VERSION"),
        game_view: cfg!(any(
            target_os = "macos",
            target_os = "android",
            target_os = "ios"
        )),
    }
}

/// Sends text to the running guest as IME characters (Runtime::post_text).
#[cfg(not(any(target_os = "macos", target_os = "android", target_os = "ios")))]
#[tauri::command]
fn post_text(text: String) -> Result<usize, String> {
    let _ = text;
    Err("no game is running".into())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(mobile)]
    adv_shell::init_system_log();
    #[cfg(target_os = "ios")]
    {
        // Before the webview, which would take the only range it fits.
        if let Err(error) = adv_shell::reserve_guest_window() {
            log::error!("guest window: {error}");
        }
        game::prepare_jit();
    }
    let builder = tauri::Builder::default()
        .manage(overlay::OverlayState::default())
        .plugin(tauri_plugin_opener::init());
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_storage::init());
    let builder = builder.setup(|app| {
        use tauri::Manager;
        #[cfg(target_os = "android")]
        android::setup(app.handle().clone());
        app.manage(std::sync::Mutex::new(library::Library::load(
            app.path().app_data_dir()?,
        )?));
        #[cfg(desktop)]
        {
            let menu = menu::build(app.handle())?;
            if cfg!(target_os = "macos") {
                app.set_menu(menu)?;
            }
        }
        #[cfg(target_os = "macos")]
        chrome::setup(app)?;
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        game::setup(app)?;
        #[cfg(target_os = "ios")]
        jit_ios::request_at_launch(app.handle());
        Ok(())
    });
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_dialog::init())
        .on_menu_event(|app, event| menu::on_event(app, event.id().as_ref()));
    #[cfg(target_os = "macos")]
    let builder =
        builder
            .on_window_event(game::window_event)
            .invoke_handler(tauri::generate_handler![
                host_info,
                settings::pick_game_file,
                settings::font_families,
                game::set_game_playback,
                game::game_fps,
                post_text,
                start_game,
                game::pause_game,
                game::set_game_filter,
                stop_game,
                overlay::set_overlay_input,
                report::report_base,
                report::save_report,
                menu::set_pause_available,
                menu::set_menu_language,
                chrome::set_title_bar,
                library::library_pick_folder,
                library::library_list,
                vndb::vndb_search,
                vndb::vndb_apply,
                vndb::vndb_unlink,
                update::check_update,
                library::set_tracking,
                library::library_inspect,
                library::library_scan,
                library::library_add,
                library::library_update,
                library::library_remove,
                library::library_restore,
                drive::reveal_drive,
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
        menu::set_menu_language,
        library::library_pick_folder,
        library::library_list,
        vndb::vndb_search,
        vndb::vndb_apply,
        vndb::vndb_unlink,
        update::check_update,
        library::set_tracking,
        library::library_inspect,
        library::library_scan,
        library::library_add,
        library::library_update,
        library::library_remove,
        library::library_restore,
        library::reveal_path
    ]);
    #[cfg(target_os = "android")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        host_info,
        post_text,
        start_game,
        stop_game,
        game::pause_game,
        game::set_game_filter,
        game::set_game_playback,
        game::game_fps,
        game::set_touch_mode,
        game::game_mouse_button,
        game::game_key,
        game::keep_game,
        game::game_frames,
        game::game_presented,
        game::game_dialog,
        game::game_menu,
        game::game_wheel,
        overlay::set_overlay_input,
        report::report_base,
        report::save_report,
        settings::pick_game_file,
        settings::font_families,
        library::library_pick_folder,
        library::library_list,
        vndb::vndb_search,
        vndb::vndb_apply,
        vndb::vndb_unlink,
        update::check_update,
        library::set_tracking,
        library::library_inspect,
        library::library_scan,
        library::library_add,
        library::library_update,
        library::library_remove,
        library::library_restore,
        drive::reveal_drive,
        library::reveal_path
    ]);
    #[cfg(target_os = "ios")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        host_info,
        post_text,
        start_game,
        stop_game,
        game::pause_game,
        game::set_game_filter,
        game::set_game_playback,
        game::game_fps,
        game::set_touch_mode,
        game::game_mouse_button,
        game::game_key,
        game::keep_game,
        game::game_frames,
        game::game_presented,
        game::game_dialog,
        game::game_menu,
        game::game_wheel,
        jit_ios::jit_status,
        jit_ios::request_jit,
        jit_ios::import_pairing_file,
        jit_ios::check_ddi,
        jit_ios::download_ddi,
        overlay::set_overlay_input,
        report::report_base,
        report::save_report,
        settings::pick_game_file,
        settings::font_families,
        library::library_pick_folder,
        library::library_list,
        vndb::vndb_search,
        vndb::vndb_apply,
        vndb::vndb_unlink,
        update::check_update,
        library::set_tracking,
        library::library_inspect,
        library::library_scan,
        library::library_add,
        library::library_update,
        library::library_remove,
        library::library_restore,
        drive::reveal_drive,
        library::reveal_path
    ]);
    let app = builder
        .build(tauri::generate_context!())
        .expect("the launcher failed to start");
    #[cfg(target_os = "macos")]
    app.run(game::run_event);
    #[cfg(not(target_os = "macos"))]
    app.run(|_, _| {});
}
