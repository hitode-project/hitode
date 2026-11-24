//! The player's Wine prefix, which every game shares as one Windows user's
//! machine. Games write their own folders in place; what they write
//! elsewhere lands on its drive C:, which the player opens from the launcher.
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

fn prefix(app: &AppHandle) -> tauri::Result<PathBuf> {
    Ok(app.path().app_data_dir()?.join("wine"))
}

/// Drive C: outside the prefix, where the system shows it: on iOS the
/// app's Documents, which the Files app lists.
fn outside(app: &AppHandle) -> tauri::Result<Option<PathBuf>> {
    #[cfg(target_os = "ios")]
    return Ok(Some(app.path().document_dir()?.join("C")));
    #[cfg(not(target_os = "ios"))]
    {
        let _ = app;
        Ok(None)
    }
}

/// Drive C: on the host.
pub(crate) fn drive(app: &AppHandle) -> tauri::Result<PathBuf> {
    Ok(match outside(app)? {
        Some(drive) => drive,
        None => prefix(app)?.join("drive_c"),
    })
}

/// Runs the game on the player's prefix.
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
pub(crate) fn apply(app: &AppHandle, options: &mut adv_shell::Options) -> Result<(), String> {
    options.wine_prefix = Some(prefix(app).map_err(|e| e.to_string())?);
    options.wine_drive = outside(app).map_err(|e| e.to_string())?;
    Ok(())
}

/// Shows drive C: in Finder or the Files app.
#[tauri::command]
pub fn reveal_drive(app: AppHandle) -> Result<(), String> {
    let drive = drive(&app).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&drive).map_err(|e| e.to_string())?;
    #[cfg(target_os = "android")]
    {
        crate::android::reveal_drive(&app)
    }
    #[cfg(target_os = "ios")]
    {
        use tauri_plugin_opener::OpenerExt;
        app.opener()
            .open_url(
                format!("shareddocuments://{}", drive.display()),
                None::<&str>,
            )
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let _ = app;
        tauri_plugin_opener::open_path(drive, None::<&str>).map_err(|e| e.to_string())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryImport {
    pub entries: usize,
    pub normalized_wow64: usize,
}

/// Imports a `.reg` file or runs a `.bat` / `.cmd` registry restore script in
/// the game's Wine environment (same prefix, drive letter, discs and code page).
#[tauri::command]
pub async fn import_registry(
    app: AppHandle,
    id: String,
    path: String,
    discs: Vec<PathBuf>,
) -> Result<RegistryImport, String> {
    if discs.len() > 4 {
        return Err("at most four disc images can be mounted".into());
    }
    let _activity = app.state::<crate::game_storage::Activity>().acquire()?;
    let (exe, data_dir, codepage) = app
        .state::<Mutex<crate::library::Library>>()
        .lock()
        .unwrap()
        .launch(&id)
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "ios")]
    let exe = {
        use tauri_plugin_storage::StorageExt;
        let folder = app.storage().prepare_folder(exe.parent().unwrap()).await?;
        folder.join(exe.file_name().unwrap())
    };
    let candidate = Path::new(&path);
    let file = if candidate.is_relative() {
        exe.parent()
            .ok_or("the program has no directory")?
            .join(candidate)
    } else {
        candidate.to_path_buf()
    };
    let mut options = adv_shell::Options::new(exe, data_dir);
    options.codepage = Some(codepage);
    options.cdroms = discs
        .into_iter()
        .enumerate()
        .map(|(i, p)| ((b'D' + i as u8) as char, p))
        .collect();
    apply(&app, &mut options)?;
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || crate::distribution::prepare_in(&app_data))
        .await
        .map_err(|e| e.to_string())??;
    let app_for_run = app.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        app_for_run
            .state::<adv_shell::Execution>()
            .restore_registry(options, file)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(RegistryImport {
        entries: report.entries,
        normalized_wow64: report.normalized_wow64,
    })
}
