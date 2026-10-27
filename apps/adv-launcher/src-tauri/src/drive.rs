//! The player's Wine prefix, which every game shares as one Windows user's
//! machine. Games write their own folders in place; what they write
//! elsewhere lands on its drive C:, which the player opens from the launcher.
use std::path::PathBuf;
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
    return crate::android::reveal_drive(&app);
    #[cfg(target_os = "ios")]
    {
        use tauri_plugin_opener::OpenerExt;
        return app
            .opener()
            .open_url(
                format!("shareddocuments://{}", drive.display()),
                None::<&str>,
            )
            .map_err(|e| e.to_string());
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let _ = app;
        tauri_plugin_opener::open_path(drive, None::<&str>).map_err(|e| e.to_string())
    }
}
