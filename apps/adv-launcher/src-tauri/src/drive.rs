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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryImport {
    pub entries: usize,
    pub normalized_wow64: usize,
}

/// Imports a `.reg` file into the game's Wine environment (same prefix,
/// drive letter, discs and code page). Scripts run as programs instead.
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
    let ((exe, data_dir, codepage), (_, program_files)) = {
        let library = app.state::<Mutex<crate::library::Library>>();
        let library = library.lock().unwrap();
        let launch = library.launch(&id).map_err(|e| e.to_string())?;
        (
            launch,
            library.installation(&id).map_err(|e| e.to_string())?,
        )
    };
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
    // An installed game keeps the `C:\Program Files` its setup wrote into scripts.
    options.install_dir = program_files;
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
            .import_registry(options, file)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(RegistryImport {
        entries: report.entries,
        normalized_wow64: report.normalized_wow64,
    })
}

/// A folder a setup installed a program into, outside every game's folder.
#[derive(Serialize)]
pub struct Installed {
    name: String,
    folder: PathBuf,
}

/// Where setups put programs the library does not hold yet, as their
/// uninstall entries say (WINE-SES-14). A setup told to install elsewhere
/// than the chosen folder leaves its game there.
#[tauri::command]
pub async fn library_installed(app: AppHandle, id: String) -> Result<Vec<Installed>, String> {
    let _activity = app.state::<crate::game_storage::Activity>().acquire()?;
    let (exe, data_dir, codepage, program_files, folders) = {
        let library = app.state::<Mutex<crate::library::Library>>();
        let library = library.lock().unwrap();
        let (exe, data_dir, codepage) = library.launch(&id).map_err(|e| e.to_string())?;
        let (_, program_files) = library.installation(&id).map_err(|e| e.to_string())?;
        let folders: Vec<PathBuf> = library
            .games
            .iter()
            .map(|game| game.folder.clone())
            .collect();
        (exe, data_dir, codepage, program_files, folders)
    };
    let mut options = adv_shell::Options::new(exe, data_dir);
    options.codepage = Some(codepage);
    options.install_dir = program_files;
    apply(&app, &mut options)?;
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || crate::distribution::prepare_in(&app_data))
        .await
        .map_err(|e| e.to_string())??;
    let app_for_run = app.clone();
    let found = tauri::async_runtime::spawn_blocking(move || {
        app_for_run
            .state::<adv_shell::Execution>()
            .installed(options)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(found
        .into_iter()
        .filter(|program| {
            !folders
                .iter()
                .any(|folder| program.folder.starts_with(folder))
        })
        .map(|program| Installed {
            name: program.name,
            folder: program.folder,
        })
        .collect())
}
