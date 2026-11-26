//! The player frontend: a Tauri window whose webview draws the game library
//! and, while a game runs, a transparent in-game overlay above the native game
//! view that adv-shell presents into.
#[cfg(target_os = "android")]
pub mod android;
#[cfg(any(target_os = "macos", test))]
mod close;
mod covers;
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
mod distribution;
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
mod drive;
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
#[cfg_attr(target_os = "android", path = "game_android.rs")]
#[cfg_attr(target_os = "ios", path = "game_ios.rs")]
mod game;
mod game_storage;
mod gamepad;
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod gamepad_apple;
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
mod haptics;
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
mod redist;
#[cfg(target_os = "ios")]
mod remote_pairing;
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
        let reserved = adv_shell::reserve_guest_window();
        if let Some(space) = adv_shell::guest_address_space() {
            log::info!("address space at launch: {space}");
        }
        if let Err(error) = reserved {
            log::error!("guest window: {error}");
        }
        game::prepare_jit();
    }
    let builder = tauri::Builder::default()
        .manage(overlay::OverlayState::default())
        .manage(game_storage::Activity::default())
        .plugin(tauri_plugin_opener::init())
        .register_uri_scheme_protocol("cover", |ctx, request| {
            use tauri::Manager;
            let response = ctx
                .app_handle()
                .path()
                .app_data_dir()
                .ok()
                .and_then(|dir| covers::read_for_protocol(&dir, request.uri().path()))
                .map(|(mime, bytes)| {
                    tauri::http::Response::builder()
                        .status(200)
                        .header(tauri::http::header::CONTENT_TYPE, mime)
                        .header(
                            tauri::http::header::CACHE_CONTROL,
                            "public, max-age=31536000, immutable",
                        )
                        .body(bytes)
                });
            match response {
                Some(Ok(reply)) => reply,
                _ => tauri::http::Response::builder()
                    .status(404)
                    .body(Vec::new())
                    .unwrap(),
            }
        });
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_storage::init());
    let builder = builder.setup(|app| {
        use tauri::Manager;
        #[cfg(target_os = "android")]
        android::setup(app.handle().clone());
        let data_dir = app.path().app_data_dir()?;
        #[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
        drive::apply_pending_reset(app.handle());
        gamepad::init(app.handle().clone(), &data_dir);
        app.manage(std::sync::Mutex::new(library::Library::load(data_dir)?));
        #[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
        app.manage(adv_shell::Execution::new(distribution::path(
            &app.path().app_data_dir()?,
        ))?);
        game_storage::startup_maintenance(app.handle());
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
                game::change_disc,
                play::script_input,
                game::set_game_filter,
                stop_game,
                overlay::set_overlay_input,
                report::report_base,
                report::save_report,
                report::report_json,
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
                library::library_add_install,
                library::library_inspect_disc,
                library::library_update,
                library::library_remove,
                library::library_restore,
                library::library_reorder,
                haptics::haptic_tick,
                game_storage::list_game_copies,
                game_storage::storage_summary,
                game_storage::clear_game_cache,
                game_storage::clear_single_game_cache,
                game_storage::list_imported_files,
                game_storage::delete_imported_file,
                game_storage::game_file_location,
                game_storage::reveal_game_files,
                game_storage::delete_game_copy,
                game_storage::export_game_copy,
                game_storage::use_game_copy,
                drive::reveal_drive,
                drive::import_registry,
                drive::library_installed,
                drive::wine_environment_status,
                drive::clean_wine_environment,
                drive::cancel_wine_reset,
                drive::restart_launcher,
                redist::install_runtime_component,
                redist::import_runtime_component,
                library::reveal_path,
                gamepad::gamepad_get_state,
                gamepad::gamepad_save_config
            ]);
    #[cfg(all(desktop, not(target_os = "macos")))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        host_info,
        post_text,
        overlay::set_overlay_input,
        report::report_base,
        report::save_report,
        report::report_json,
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
        library::library_add_install,
        library::library_inspect_disc,
        library::library_update,
        library::library_remove,
        library::library_restore,
        library::library_reorder,
        game_storage::list_game_copies,
        game_storage::storage_summary,
        game_storage::clear_game_cache,
        game_storage::clear_single_game_cache,
        game_storage::list_imported_files,
        game_storage::delete_imported_file,
        game_storage::game_file_location,
        game_storage::reveal_game_files,
        game_storage::delete_game_copy,
        game_storage::export_game_copy,
        game_storage::use_game_copy,
        library::reveal_path,
        gamepad::gamepad_get_state,
        gamepad::gamepad_save_config
    ]);
    #[cfg(target_os = "android")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        host_info,
        android::open_issue_in_browser,
        android::set_launcher_back,
        report::share_report,
        post_text,
        start_game,
        stop_game,
        game::pause_game,
        game::change_disc,
        play::script_input,
        game::set_game_filter,
        game::set_game_playback,
        game::game_fps,
        game::set_touch_mode,
        game::game_mouse_button,
        game::game_key,
        game::game_frames,
        game::game_presented,
        game::game_dialog,
        game::game_menu,
        game::game_wheel,
        overlay::set_overlay_input,
        report::report_base,
        report::save_report,
        report::report_json,
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
        library::library_add_install,
        library::library_inspect_disc,
        library::library_update,
        library::library_remove,
        library::library_restore,
        haptics::haptic_tick,
        library::library_reorder,
        game_storage::list_game_copies,
        game_storage::storage_summary,
        game_storage::clear_game_cache,
        game_storage::clear_single_game_cache,
        game_storage::list_imported_files,
        game_storage::delete_imported_file,
        game_storage::game_file_location,
        game_storage::reveal_game_files,
        game_storage::delete_game_copy,
        game_storage::export_game_copy,
        game_storage::use_game_copy,
        drive::reveal_drive,
        drive::import_registry,
        drive::library_installed,
        drive::wine_environment_status,
        drive::clean_wine_environment,
        drive::cancel_wine_reset,
        redist::install_runtime_component,
        redist::import_runtime_component,
        library::reveal_path,
        gamepad::gamepad_get_state,
        gamepad::gamepad_save_config
    ]);
    #[cfg(target_os = "ios")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        host_info,
        post_text,
        start_game,
        stop_game,
        game::pause_game,
        game::change_disc,
        play::script_input,
        game::set_game_filter,
        game::set_game_playback,
        game::game_fps,
        game::set_touch_mode,
        game::game_mouse_button,
        game::game_key,
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
        game::set_launcher_back,
        report::share_report,
        overlay::set_overlay_input,
        report::report_base,
        report::save_report,
        report::report_json,
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
        library::library_add_install,
        library::library_inspect_disc,
        library::library_update,
        library::library_remove,
        haptics::haptic_tick,
        library::library_restore,
        library::library_reorder,
        game_storage::list_game_copies,
        game_storage::storage_summary,
        game_storage::clear_game_cache,
        game_storage::clear_single_game_cache,
        game_storage::list_imported_files,
        game_storage::delete_imported_file,
        game_storage::game_file_location,
        game_storage::reveal_game_files,
        game_storage::delete_game_copy,
        game_storage::export_game_copy,
        game_storage::use_game_copy,
        drive::reveal_drive,
        drive::import_registry,
        drive::library_installed,
        drive::wine_environment_status,
        drive::clean_wine_environment,
        drive::cancel_wine_reset,
        redist::install_runtime_component,
        redist::import_runtime_component,
        library::reveal_path,
        gamepad::gamepad_get_state,
        gamepad::gamepad_save_config
    ]);
    let app = builder
        .build(tauri::generate_context!())
        .expect("the launcher failed to start");
    #[cfg(target_os = "macos")]
    app.run(game::run_event);
    #[cfg(not(target_os = "macos"))]
    app.run(|_, _| {});
}
