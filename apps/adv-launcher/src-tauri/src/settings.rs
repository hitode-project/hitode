//! Launcher file selection and native font discovery.
use std::path::PathBuf;
#[cfg(desktop)]
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
#[cfg(desktop)]
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

/// The system picker shows every file, so the extension is checked here.
/// Copies live beside the library, out of the iOS picker's temporary inbox.
#[tauri::command]
#[cfg(mobile)]
pub async fn pick_game_file(app: tauri::AppHandle, kind: String) -> Result<Option<String>, String> {
    use tauri::Manager;
    use tauri_plugin_storage::StorageExt;
    let extensions: &[&str] = match kind.as_str() {
        "disc" => &["iso"],
        "font" => &["ttf", "ttc", "otf", "otc"],
        _ => return Err("unknown file type".into()),
    };
    let files = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("files");
    std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
    let Some(mut path) = app.storage().pick_file(&files).await?.path else {
        return Ok(None);
    };
    let matches = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.contains(&e.to_ascii_lowercase().as_str()));
    if !matches {
        return Err(format!("choose a .{} file", extensions.join(" / .")));
    }
    if cfg!(target_os = "ios") {
        let folder = fresh_folder(&files)?;
        let target = folder.join(path.file_name().ok_or("the file has no name")?);
        std::fs::rename(&path, &target).map_err(|e| e.to_string())?;
        path = target;
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// A new folder under `parent`, so a kept copy keeps its original name.
#[cfg(mobile)]
fn fresh_folder(parent: &std::path::Path) -> Result<PathBuf, String> {
    for attempt in 0..100u32 {
        let folder = parent.join(format!("file-{}-{attempt}", crate::library::now()));
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("cannot create a folder for the file".into())
}

#[tauri::command]
pub async fn font_families(path: Option<PathBuf>) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // The game's renderer discovers installed fonts, including Android's.
        let Some(path) = path else {
            return Ok(adv_shell::system_font_families());
        };
        let mut db = fontdb::Database::new();
        db.load_font_file(path).map_err(|e| e.to_string())?;
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
