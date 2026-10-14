//! Launcher file selection and native font discovery.
use std::path::PathBuf;
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub async fn pick_game_file(app: tauri::AppHandle, kind: String) -> Result<Option<String>, String> {
    let picker = app.dialog().file();
    let picker = match kind.as_str() {
        "disc" => picker.add_filter("Disc image", &["iso"]),
        "font" => picker.add_filter("Font", &["ttf", "ttc", "otf", "otc"]),
        _ => return Err("unknown file type".into()),
    };
    tauri::async_runtime::spawn_blocking(move || {
        picker
            .blocking_pick_file()
            .map(|file| {
                file.into_path()
                    .map(|p| p.to_string_lossy().into_owned())
                    .map_err(|e| e.to_string())
            })
            .transpose()
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn font_families(path: Option<PathBuf>) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut db = fontdb::Database::new();
        if let Some(path) = path {
            db.load_font_file(path).map_err(|e| e.to_string())?;
        } else {
            db.load_system_fonts();
        }
        let mut names: Vec<_> = db
            .faces()
            .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    })
    .await
    .map_err(|e| e.to_string())?
}
