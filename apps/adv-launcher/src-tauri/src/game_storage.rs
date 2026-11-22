//! Managed game copies and rebuildable caches (HOST-LIB-7, HOST-LIB-8).
use crate::library::Library;
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Manager};

type Reply<T> = Result<T, String>;

/// An import directory cannot be deleted or exported while a game can use it.
#[derive(Default)]
pub(crate) struct Activity(Arc<AtomicBool>);

pub(crate) struct Lease(Arc<AtomicBool>);

impl Activity {
    pub(crate) fn acquire(&self) -> Reply<Lease> {
        if self.0.swap(true, Ordering::AcqRel) {
            return Err("A game or file operation is already running.".into());
        }
        Ok(Lease(self.0.clone()))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GameCopy {
    id: String,
    name: String,
    directory: PathBuf,
    bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Location {
    kind: &'static str,
    display_path: PathBuf,
    can_reveal: bool,
    copy: Option<GameCopy>,
    cache_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageSummary {
    data_dir: PathBuf,
    game_files_bytes: u64,
    env_bytes: u64,
    cache_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportedFile {
    id: String,
    name: String,
    kind: &'static str,
    path: PathBuf,
    bytes: u64,
}

#[derive(Serialize)]
pub(crate) struct Export {
    label: String,
    path: Option<PathBuf>,
}

fn imports(app: &AppHandle) -> io::Result<PathBuf> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(io::Error::other)?
        .join("imports"))
}

fn files_dir(app: &AppHandle) -> io::Result<PathBuf> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(io::Error::other)?
        .join("files"))
}

fn real_directory(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.is_dir()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn clear_legacy_gpu_caches(games: &Path) -> io::Result<()> {
    if !real_directory(games)? {
        return Ok(());
    }
    for game in fs::read_dir(games)? {
        let game = game?;
        if !game.file_type()?.is_dir() {
            continue;
        }
        let artifacts = game.path().join("artifacts");
        let gpu = artifacts.join("gpu");
        if real_directory(&artifacts)? && real_directory(&gpu)? {
            fs::remove_dir_all(gpu)?;
        }
    }
    Ok(())
}

/// Removes crashed mobile import staging directories (`imports/.*`) without following symlinks.
fn prune_import_staging(imports_dir: &Path) -> io::Result<()> {
    if !real_directory(imports_dir)? {
        return Ok(());
    }
    for entry in fs::read_dir(imports_dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() || !kind.is_dir() {
            continue;
        }
        if entry.file_name().to_string_lossy().starts_with('.') {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

/// Removes crashed Wine mount temporary directories (`wine/.hitode-files-*`) without following symlinks.
fn prune_wine_mount_temps(wine_dir: &Path) -> io::Result<()> {
    if !real_directory(wine_dir)? {
        return Ok(());
    }
    for entry in fs::read_dir(wine_dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() || !kind.is_dir() {
            continue;
        }
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".hitode-files-")
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

/// Removes GPU cache subdirectories for games no longer in the library, keeping
/// active game caches and shared driver health files intact.
fn prune_orphan_gpu_caches(gpu_root: &Path, known_games: &HashSet<String>) -> io::Result<()> {
    if !real_directory(gpu_root)? {
        return Ok(());
    }
    for entry in fs::read_dir(gpu_root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() || !kind.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(id) = name.to_str() else {
            continue;
        };
        if !known_games.contains(id) {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

fn clear_caches(
    data_dir: &Path,
    cache_dir: &Path,
    current_runtime_id: Option<&str>,
) -> io::Result<()> {
    let gpu = cache_dir.join("gpu");
    if real_directory(&gpu)? {
        fs::remove_dir_all(gpu)?;
    }
    clear_legacy_gpu_caches(&data_dir.join("games"))?;
    #[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
    if let Some(id) = current_runtime_id {
        crate::distribution::prune_stale(&data_dir.join("runtime"), id)?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "android", target_os = "ios")))]
    let _ = current_runtime_id;
    prune_import_staging(&data_dir.join("imports"))?;
    prune_wine_mount_temps(&data_dir.join("wine"))?;
    Ok(())
}

fn run_startup_maintenance(
    data_dir: &Path,
    cache_dir: &Path,
    known_games: &HashSet<String>,
    referenced_covers: &HashSet<String>,
    current_runtime_id: Option<&str>,
) {
    #[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
    if let Some(id) = current_runtime_id {
        let _ = crate::distribution::prune_stale(&data_dir.join("runtime"), id);
    }
    #[cfg(not(any(target_os = "macos", target_os = "android", target_os = "ios")))]
    let _ = current_runtime_id;
    let _ = prune_import_staging(&data_dir.join("imports"));
    let _ = prune_wine_mount_temps(&data_dir.join("wine"));
    let _ = clear_legacy_gpu_caches(&data_dir.join("games"));
    let _ = prune_orphan_gpu_caches(&cache_dir.join("gpu"), known_games);
    let _ = crate::covers::prune_orphans(data_dir, referenced_covers);
    #[cfg(target_os = "ios")]
    mark_backup_exclusions(data_dir);
}

#[cfg(target_os = "ios")]
pub(crate) fn exclude_from_backup(path: &Path) {
    use objc2_foundation::{NSNumber, NSString, NSURL, NSURLIsExcludedFromBackupKey};
    let Some(text) = path.to_str() else {
        return;
    };
    let url = NSURL::fileURLWithPath(&NSString::from_str(text));
    let value = NSNumber::new_bool(true);
    // SAFETY: NSURLIsExcludedFromBackupKey expects a boolean NSNumber.
    let _ = unsafe {
        url.setResourceValue_forKey_error(Some(value.as_ref()), NSURLIsExcludedFromBackupKey)
    };
}

/// Excludes large rebuildable or re-importable directories from iCloud backup.
#[cfg(target_os = "ios")]
pub(crate) fn mark_backup_exclusions(data_dir: &Path) {
    for name in ["runtime", "imports", "files", crate::covers::DIR] {
        let dir = data_dir.join(name);
        if real_directory(&dir).unwrap_or(false) {
            exclude_from_backup(&dir);
        }
    }
}

/// Quietly sweeps obsolete runtimes, crash leftovers, legacy caches and orphan covers on startup.
pub(crate) fn startup_maintenance(app: &AppHandle) {
    let Ok(data_dir) = app.path().app_data_dir() else {
        return;
    };
    let Ok(cache_dir) = app.path().app_cache_dir() else {
        return;
    };
    let (known_games, referenced_covers) = {
        let state = app.state::<Mutex<Library>>();
        let library = state.lock().unwrap();
        (library.game_ids(), library.referenced_covers())
    };
    let Ok(files) = app.state::<Activity>().acquire() else {
        return;
    };
    #[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
    let current_runtime_id = crate::distribution::current_id();
    #[cfg(not(any(target_os = "macos", target_os = "android", target_os = "ios")))]
    let current_runtime_id: Option<&str> = None;
    tauri::async_runtime::spawn_blocking(move || {
        let _files = files;
        run_startup_maintenance(
            &data_dir,
            &cache_dir,
            &known_games,
            &referenced_covers,
            current_runtime_id,
        );
    });
}

fn real_dir_bytes(path: &Path) -> io::Result<u64> {
    if !real_directory(path)? {
        return Ok(0);
    }
    folder_bytes(path)
}

fn single_game_cache_bytes(data_dir: &Path, cache_dir: &Path, game_id: &str) -> io::Result<u64> {
    if !copy_id(game_id) {
        return Ok(0);
    }
    let current = real_dir_bytes(&cache_dir.join("gpu").join(game_id))?;
    let legacy = real_dir_bytes(&data_dir.join("games").join(game_id).join("artifacts/gpu"))?;
    Ok(current.saturating_add(legacy))
}

fn clear_one_game_cache(data_dir: &Path, cache_dir: &Path, game_id: &str) -> io::Result<()> {
    if !copy_id(game_id) {
        return Err(io::Error::other("Invalid game identifier"));
    }
    let current = cache_dir.join("gpu").join(game_id);
    if real_directory(&current)? {
        fs::remove_dir_all(current)?;
    }
    let artifacts = data_dir.join("games").join(game_id).join("artifacts");
    let legacy = artifacts.join("gpu");
    if real_directory(&artifacts)? && real_directory(&legacy)? {
        fs::remove_dir_all(legacy)?;
    }
    Ok(())
}

fn measure_storage(
    data_dir: &Path,
    cache_dir: &Path,
    ios_drive: Option<&Path>,
    current_runtime_id: Option<&str>,
) -> io::Result<StorageSummary> {
    let mut game_files_bytes = real_dir_bytes(&data_dir.join("files"))?;
    let mut env_bytes = real_dir_bytes(&data_dir.join(crate::covers::DIR))?;
    let mut cache_bytes = real_dir_bytes(&cache_dir.join("gpu"))?;

    if let Ok(meta) = fs::symlink_metadata(data_dir.join("library.json"))
        && meta.is_file()
    {
        env_bytes = env_bytes.saturating_add(meta.len());
    }

    let imports_root = data_dir.join("imports");
    if real_directory(&imports_root)? {
        for entry in fs::read_dir(&imports_root)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() || !kind.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with('.') {
                cache_bytes = cache_bytes.saturating_add(folder_bytes(&entry.path())?);
            } else if copy_id(name) {
                game_files_bytes = game_files_bytes.saturating_add(folder_bytes(&entry.path())?);
            }
        }
    }

    let wine_root = data_dir.join("wine");
    if real_directory(&wine_root)? {
        for entry in fs::read_dir(&wine_root)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let is_mount_temp = name.to_string_lossy().starts_with(".hitode-files-");
            let bytes = if kind.is_dir() {
                folder_bytes(&entry.path())?
            } else if kind.is_file() {
                entry.metadata()?.len()
            } else {
                0
            };
            if is_mount_temp && kind.is_dir() {
                cache_bytes = cache_bytes.saturating_add(bytes);
            } else {
                env_bytes = env_bytes.saturating_add(bytes);
            }
        }
    }

    if let Some(drive) = ios_drive {
        env_bytes = env_bytes.saturating_add(real_dir_bytes(drive)?);
    }

    let runtime_root = data_dir.join("runtime");
    if real_directory(&runtime_root)? {
        for entry in fs::read_dir(&runtime_root)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let bytes = if kind.is_dir() {
                folder_bytes(&entry.path())?
            } else if kind.is_file() {
                entry.metadata()?.len()
            } else {
                0
            };
            let is_current = current_runtime_id
                .is_none_or(|keep| entry.file_name().to_str() == Some(keep) && kind.is_dir());
            if is_current {
                env_bytes = env_bytes.saturating_add(bytes);
            } else {
                cache_bytes = cache_bytes.saturating_add(bytes);
            }
        }
    }

    let games_root = data_dir.join("games");
    if real_directory(&games_root)? {
        for game in fs::read_dir(&games_root)? {
            let game = game?;
            if !game.file_type()?.is_dir() {
                continue;
            }
            let artifacts = game.path().join("artifacts");
            let gpu = artifacts.join("gpu");
            if real_directory(&artifacts)? && real_directory(&gpu)? {
                cache_bytes = cache_bytes.saturating_add(folder_bytes(&gpu)?);
            }
        }
    }

    Ok(StorageSummary {
        data_dir: data_dir.to_path_buf(),
        game_files_bytes,
        env_bytes,
        cache_bytes,
    })
}

fn imported_file_kind(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("iso") => "disc",
        Some("ttf" | "ttc" | "otf" | "otc") => "font",
        _ => "other",
    }
}

fn list_files(root: &Path) -> io::Result<Vec<ImportedFile>> {
    if !real_directory(root)? {
        return Ok(Vec::new());
    }
    let root = root.canonicalize()?;
    let mut out = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() || !kind.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if !copy_id(&id) {
            continue;
        }
        let folder = entry.path();
        let bytes = folder_bytes(&folder)?;
        let mut first_file = None;
        for child in fs::read_dir(&folder)? {
            let child = child?;
            if child.file_type()?.is_file() {
                first_file = Some(child.path());
                break;
            }
        }
        let Some(path) = first_file else {
            continue;
        };
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        out.push(ImportedFile {
            id,
            kind: imported_file_kind(&path),
            name,
            path,
            bytes,
        });
    }
    out.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

fn delete_file(root: &Path, id: &str) -> io::Result<()> {
    if !copy_id(id) {
        return Err(io::Error::other("Invalid file identifier"));
    }
    let root = import_root(root)?;
    let folder = root.join(id);
    if !fs::symlink_metadata(&folder)?.is_dir() {
        return Err(io::Error::other("The imported file folder is missing"));
    }
    fs::remove_dir_all(folder)
}

#[tauri::command]
pub(crate) async fn storage_summary(app: AppHandle) -> Reply<StorageSummary> {
    let data_dir = app.path().app_data_dir().map_err(error)?;
    let cache_dir = app.path().app_cache_dir().map_err(error)?;
    #[cfg(target_os = "ios")]
    let ios_drive = app.path().document_dir().ok().map(|d| d.join("C"));
    #[cfg(not(target_os = "ios"))]
    let ios_drive: Option<PathBuf> = None;
    #[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
    let current_runtime_id = crate::distribution::current_id();
    #[cfg(not(any(target_os = "macos", target_os = "android", target_os = "ios")))]
    let current_runtime_id: Option<&str> = None;
    worker(move || {
        measure_storage(
            &data_dir,
            &cache_dir,
            ios_drive.as_deref(),
            current_runtime_id,
        )
    })
    .await
}

#[tauri::command]
pub(crate) async fn clear_single_game_cache(app: AppHandle, game_id: String) -> Reply<()> {
    let files = app.state::<Activity>().acquire()?;
    let data_dir = app.path().app_data_dir().map_err(error)?;
    let cache_dir = app.path().app_cache_dir().map_err(error)?;
    worker(move || {
        let _files = files;
        clear_one_game_cache(&data_dir, &cache_dir, &game_id)
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_imported_files(app: AppHandle) -> Reply<Vec<ImportedFile>> {
    let root = files_dir(&app).map_err(error)?;
    worker(move || list_files(&root)).await
}

#[tauri::command]
pub(crate) async fn delete_imported_file(app: AppHandle, id: String) -> Reply<()> {
    let files = app.state::<Activity>().acquire()?;
    let root = files_dir(&app).map_err(error)?;
    worker(move || {
        let _files = files;
        delete_file(&root, &id)
    })
    .await
}

#[tauri::command]
pub(crate) async fn clear_game_cache(app: AppHandle) -> Reply<()> {
    let files = app.state::<Activity>().acquire()?;
    let data_dir = app.path().app_data_dir().map_err(error)?;
    let cache_dir = app.path().app_cache_dir().map_err(error)?;
    #[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
    let current_runtime_id = crate::distribution::current_id();
    #[cfg(not(any(target_os = "macos", target_os = "android", target_os = "ios")))]
    let current_runtime_id: Option<&str> = None;
    worker(move || {
        let _files = files;
        clear_caches(&data_dir, &cache_dir, current_runtime_id)
    })
    .await
}

fn copy_id(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn import_root(root: &Path) -> io::Result<PathBuf> {
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err(io::Error::other("imports must be a directory"));
    }
    root.canonicalize()
}

fn checked_copy(root: &Path, id: &str) -> io::Result<PathBuf> {
    if !copy_id(id) {
        return Err(io::Error::other("Invalid copy identifier"));
    }
    let folder = import_root(root)?.join(id);
    if !fs::symlink_metadata(&folder)?.is_dir() {
        return Err(io::Error::other("The copy is not a directory"));
    }
    Ok(folder)
}

fn export_source(folder: &Path) -> io::Result<PathBuf> {
    let mut entries = fs::read_dir(folder)?;
    let first = entries.next().transpose()?;
    if let Some(first) = first
        && first.file_type()?.is_dir()
        && entries.next().transpose()?.is_none()
    {
        return Ok(first.path());
    }
    Ok(folder.to_path_buf())
}

fn copy_name(folder: &Path) -> io::Result<String> {
    Ok(export_source(folder)?
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned())
}

fn folder_bytes(folder: &Path) -> io::Result<u64> {
    let mut bytes = 0;
    for item in fs::read_dir(folder)? {
        let item = item?;
        let kind = item.file_type()?;
        if kind.is_dir() {
            bytes += folder_bytes(&item.path())?;
        } else if kind.is_file() {
            bytes += item.metadata()?.len();
        }
    }
    Ok(bytes)
}

fn describe(id: String, directory: PathBuf) -> io::Result<GameCopy> {
    Ok(GameCopy {
        id,
        name: copy_name(&directory)?,
        bytes: folder_bytes(&directory)?,
        directory,
    })
}

#[cfg(target_os = "ios")]
fn check_export_tree(folder: &Path) -> io::Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            check_export_tree(&entry.path())?;
        } else if !kind.is_file() {
            return Err(io::Error::other(
                "A copy cannot contain links or special files",
            ));
        }
    }
    Ok(())
}

fn list(root: &Path) -> io::Result<Vec<GameCopy>> {
    let root = match import_root(root) {
        Ok(root) => root,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut copies = Vec::new();
    for item in fs::read_dir(root)? {
        let item = item?;
        let id = item.file_name().to_string_lossy().into_owned();
        if !copy_id(&id) || !item.file_type()?.is_dir() {
            continue;
        }
        copies.push(describe(id, item.path())?);
    }
    copies.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.id.cmp(&b.id)));
    Ok(copies)
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

async fn worker<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Reply<T> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(error)?
        .map_err(error)
}

#[tauri::command]
pub(crate) async fn list_game_copies(app: AppHandle) -> Reply<Vec<GameCopy>> {
    let root = imports(&app).map_err(error)?;
    worker(move || list(&root)).await
}

fn game_folder(app: &AppHandle, id: &str) -> Reply<PathBuf> {
    Ok(app
        .state::<Mutex<Library>>()
        .lock()
        .unwrap()
        .metadata_target(id)
        .map_err(error)?
        .folder)
}

fn relative_copy(root: &Path, folder: &Path) -> Option<PathBuf> {
    let root = import_root(root).ok()?;
    let relative = folder.strip_prefix(root).ok()?;
    let Some(Component::Normal(id)) = relative.components().next() else {
        return None;
    };
    if !copy_id(id.to_str()?) {
        return None;
    }
    Some(relative.to_path_buf())
}

fn location(app: &AppHandle, folder: &Path) -> Reply<Location> {
    let root = imports(app).map_err(error)?;
    if let Some(relative) = relative_copy(&root, folder) {
        return Ok(Location {
            kind: "copy",
            display_path: Path::new("Hitode/imports").join(relative),
            can_reveal: folder.is_dir() && !cfg!(target_os = "ios"),
            copy: None,
            cache_bytes: 0,
        });
    }
    #[cfg(target_os = "ios")]
    if let Some(documents) = app
        .path()
        .document_dir()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        && let Ok(relative) = folder.strip_prefix(&documents)
    {
        return Ok(Location {
            kind: "documents",
            display_path: Path::new("Hitode").join(relative),
            can_reveal: folder.is_dir(),
            copy: None,
            cache_bytes: 0,
        });
    }
    Ok(Location {
        kind: "external",
        display_path: folder.to_path_buf(),
        can_reveal: folder.is_dir()
            && (cfg!(desktop)
                || (cfg!(target_os = "android")
                    && crate::library::android_external_volume(folder).is_some())),
        copy: None,
        cache_bytes: 0,
    })
}

#[tauri::command]
pub(crate) async fn game_file_location(app: AppHandle, game_id: String) -> Reply<Location> {
    let folder = game_folder(&app, &game_id)?;
    let mut location = location(&app, &folder)?;
    let data_dir = app.path().app_data_dir().map_err(error)?;
    let cache_dir = app.path().app_cache_dir().map_err(error)?;
    let cache_id = game_id.clone();
    location.cache_bytes =
        worker(move || single_game_cache_bytes(&data_dir, &cache_dir, &cache_id)).await?;
    if location.kind == "copy" && folder.is_dir() {
        let root = imports(&app).map_err(error)?;
        if let Some(relative) = relative_copy(&root, &folder) {
            let id = relative
                .components()
                .next()
                .unwrap()
                .as_os_str()
                .to_string_lossy()
                .into_owned();
            location.copy =
                Some(worker(move || describe(id.clone(), checked_copy(&root, &id)?)).await?);
        }
    }
    Ok(location)
}

#[tauri::command]
pub(crate) async fn reveal_game_files(
    app: AppHandle,
    game_id: Option<String>,
    copy_id: Option<String>,
) -> Reply<()> {
    let root = imports(&app).map_err(error)?;
    let folder = match (game_id, copy_id) {
        (Some(id), None) => game_folder(&app, &id)?,
        (None, Some(id)) => checked_copy(&root, &id).map_err(error)?,
        _ => return Err("Select a game or copy".into()),
    };
    let folder = folder.canonicalize().map_err(error)?;
    let location = location(&app, &folder)?;
    if !location.can_reveal {
        return Err("The system file manager cannot open this folder".into());
    }
    #[cfg(desktop)]
    return tauri_plugin_opener::open_path(folder, None::<&str>).map_err(error);
    #[cfg(target_os = "android")]
    {
        if let Some(relative) = relative_copy(&root, &folder) {
            crate::android::reveal_import(&app, &relative).await
        } else {
            crate::android::reveal_folder(&app, &folder).await
        }
    }
    #[cfg(target_os = "ios")]
    {
        use tauri_plugin_opener::OpenerExt;
        let url = tauri::Url::from_directory_path(folder).map_err(|()| "Invalid folder")?;
        app.opener()
            .open_url(format!("shareddocuments://{}", url.path()), None::<&str>)
            .map_err(error)
    }
}

fn delete_copy(root: &Path, id: &str) -> io::Result<()> {
    fs::remove_dir_all(checked_copy(root, id)?)
}

#[tauri::command]
pub(crate) async fn delete_game_copy(app: AppHandle, id: String) -> Reply<()> {
    let files = app.state::<Activity>().acquire()?;
    let root = imports(&app).map_err(error)?;
    worker(move || {
        let _files = files;
        delete_copy(&root, &id)
    })
    .await
}

#[cfg(desktop)]
fn copy_tree(source: &Path, dest: &Path) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = dest.join(entry.file_name());
        if kind.is_dir() {
            fs::create_dir(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target)?;
        } else {
            return Err(io::Error::other(
                "A copy cannot contain links or special files",
            ));
        }
    }
    Ok(())
}

#[cfg(desktop)]
fn copy_out(source: &Path, parent: &Path, name: &str) -> io::Result<PathBuf> {
    let parent = parent.canonicalize()?;
    if parent.starts_with(source) {
        return Err(io::Error::other("Choose a location outside the source"));
    }
    let target = parent.join(name);
    fs::create_dir(&target)?;
    if let Err(error) = copy_tree(source, &target) {
        fs::remove_dir_all(target)?;
        return Err(error);
    }
    Ok(target)
}

#[tauri::command]
pub(crate) async fn export_game_copy(app: AppHandle, id: String) -> Reply<Option<Export>> {
    let _files = app.state::<Activity>().acquire()?;
    let root = imports(&app).map_err(error)?;
    let source = checked_copy(&root, &id).map_err(error)?;
    let source = export_source(&source).map_err(error)?;
    let name = source.file_name().unwrap().to_string_lossy().into_owned();
    #[cfg(desktop)]
    {
        let Some(parent) = crate::library::library_pick_folder(app).await? else {
            return Ok(None);
        };
        let path = worker(move || {
            if parent.canonicalize()?.starts_with(import_root(&root)?) {
                return Err(io::Error::other("Choose a destination outside Hitode"));
            }
            copy_out(&source, &parent, &name)
        })
        .await?;
        Ok(Some(Export {
            label: path.display().to_string(),
            path: Some(path),
        }))
    }
    #[cfg(mobile)]
    {
        use tauri_plugin_storage::StorageExt;
        #[cfg(target_os = "ios")]
        {
            let path = source.clone();
            worker(move || check_export_tree(&path)).await?;
        }
        Ok(app
            .storage()
            .export_folder(&source, &name)
            .await?
            .map(|export| Export {
                label: export.label,
                path: export.path,
            }))
    }
}

fn use_copy(library: &mut Library, source: &Path, destination: &Path) -> io::Result<()> {
    let destination = destination.canonicalize()?;
    if destination.starts_with(source) || source.starts_with(&destination) {
        return Err(io::Error::other("Choose a location outside the source"));
    }
    let payload = export_source(source)?;
    let mut changes = Vec::new();
    for (i, game) in library.games.iter().enumerate() {
        let Ok(relative) = game.folder.strip_prefix(source) else {
            continue;
        };
        let relative = source.join(relative);
        let relative = relative.strip_prefix(&payload).map_err(io::Error::other)?;
        let target = destination.join(relative).canonicalize()?;
        let exe = target.join(&game.exe).canonicalize()?;
        if !target.starts_with(&destination) || !exe.starts_with(&target) || !exe.is_file() {
            return Err(io::Error::other(
                "The exported copy is missing a game executable",
            ));
        }
        changes.push((i, target));
    }
    let old: Vec<_> = changes
        .into_iter()
        .map(|(i, path)| (i, std::mem::replace(&mut library.games[i].folder, path)))
        .collect();
    if let Err(error) = library.save() {
        for (i, path) in old {
            library.games[i].folder = path;
        }
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn use_game_copy(app: AppHandle, id: String, destination: PathBuf) -> Reply<()> {
    let files = app.state::<Activity>().acquire()?;
    let root = imports(&app).map_err(error)?;
    worker(move || {
        let _files = files;
        let source = checked_copy(&root, &id)?;
        let destination = destination.canonicalize()?;
        if destination.starts_with(import_root(&root)?) {
            return Err(io::Error::other(
                "The new location must be outside managed storage",
            ));
        }
        use_copy(
            &mut app.state::<Mutex<Library>>().lock().unwrap(),
            &source,
            &destination,
        )
    })
    .await
}

#[cfg(all(test, desktop))]
mod tests {
    use super::*;
    use crate::library::LibraryGame;

    // spec: HOST-LIB-8
    #[test]
    fn clearing_cache_keeps_game_data_and_does_not_follow_links() {
        let base = std::env::temp_dir().join(format!("hitode-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir(&base).unwrap();
        let data = base.join("data");
        let cache = base.join("cache");
        let games = data.join("games");
        let artifacts = games.join("first/artifacts");
        let legacy_gpu = artifacts.join("gpu");
        let cache_gpu = cache.join("gpu");
        let outside = base.join("outside");
        fs::create_dir_all(&legacy_gpu).unwrap();
        fs::create_dir_all(cache_gpu.join("first")).unwrap();
        fs::create_dir_all(outside.join("gpu")).unwrap();
        fs::create_dir_all(data.join("imports/.staging-123")).unwrap();
        fs::create_dir_all(data.join("imports/valid-copy")).unwrap();
        fs::create_dir_all(data.join("wine/.hitode-files-999")).unwrap();
        fs::create_dir_all(data.join("wine/drive_c")).unwrap();
        fs::create_dir_all(data.join("runtime/current")).unwrap();
        fs::create_dir_all(data.join("runtime/stale")).unwrap();
        fs::create_dir_all(data.join("covers")).unwrap();
        let save = games.join("first/save.dat");
        fs::write(&save, b"save").unwrap();
        fs::write(artifacts.join("trace"), b"trace").unwrap();
        fs::write(legacy_gpu.join("shader.wgsl-cache"), b"cache").unwrap();
        fs::write(cache_gpu.join("first/shader.wgsl-cache"), b"cache").unwrap();
        fs::write(cache_gpu.join("vulkan-health.bin"), b"health").unwrap();
        fs::write(data.join("covers/v17-0123456789abcdef.png"), b"cover").unwrap();
        fs::write(data.join("wine/drive_c/save.sav"), b"c-save").unwrap();
        #[cfg(unix)]
        {
            fs::create_dir(games.join("linked")).unwrap();
            std::os::unix::fs::symlink(&outside, games.join("linked/artifacts")).unwrap();
            std::os::unix::fs::symlink(&outside, legacy_gpu.join("link")).unwrap();
            std::os::unix::fs::symlink(&outside, data.join("imports/.linked-staging")).unwrap();
            std::os::unix::fs::symlink(&outside, data.join("wine/.hitode-files-linked")).unwrap();
        }

        clear_caches(&data, &cache, Some("current")).unwrap();
        clear_caches(&data, &cache, Some("current")).unwrap();
        assert!(!legacy_gpu.exists());
        assert!(!cache_gpu.exists());
        assert!(!data.join("imports/.staging-123").exists());
        assert!(data.join("imports/valid-copy").is_dir());
        assert!(!data.join("wine/.hitode-files-999").exists());
        assert_eq!(
            fs::read(data.join("wine/drive_c/save.sav")).unwrap(),
            b"c-save"
        );
        assert!(data.join("runtime/current").is_dir());
        #[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
        assert!(!data.join("runtime/stale").exists());
        assert_eq!(
            fs::read(data.join("covers/v17-0123456789abcdef.png")).unwrap(),
            b"cover"
        );
        assert_eq!(fs::read(save).unwrap(), b"save");
        assert!(artifacts.join("trace").is_file());
        assert!(outside.join("gpu").is_dir());
        fs::remove_dir_all(base).unwrap();
    }

    // spec: HOST-LIB-8
    #[test]
    fn startup_maintenance_keeps_active_caches_and_prunes_orphans() {
        let base = std::env::temp_dir().join(format!("hitode-maint-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let data = base.join("data");
        let cache = base.join("cache");
        fs::create_dir_all(cache.join("gpu/active")).unwrap();
        fs::create_dir_all(cache.join("gpu/removed")).unwrap();
        fs::create_dir_all(data.join("covers")).unwrap();
        fs::create_dir_all(data.join("imports/.crashed-1")).unwrap();
        fs::create_dir_all(data.join("wine/.hitode-files-42")).unwrap();
        fs::write(cache.join("gpu/vulkan-health.bin"), b"health").unwrap();
        fs::write(cache.join("gpu/active/a.native"), b"keep").unwrap();
        fs::write(cache.join("gpu/removed/b.native"), b"drop").unwrap();
        fs::write(data.join("covers/v1-0123456789abcdef.png"), b"keep").unwrap();
        fs::write(data.join("covers/v2-fedcba9876543210.png"), b"drop").unwrap();

        let known_games = HashSet::from(["active".to_owned()]);
        let known_covers = HashSet::from(["v1-0123456789abcdef.png".to_owned()]);
        run_startup_maintenance(&data, &cache, &known_games, &known_covers, None);

        assert!(cache.join("gpu/vulkan-health.bin").is_file());
        assert!(cache.join("gpu/active/a.native").is_file());
        assert!(!cache.join("gpu/removed").exists());
        assert!(data.join("covers/v1-0123456789abcdef.png").is_file());
        assert!(!data.join("covers/v2-fedcba9876543210.png").exists());
        assert!(!data.join("imports/.crashed-1").exists());
        assert!(!data.join("wine/.hitode-files-42").exists());
        fs::remove_dir_all(base).unwrap();
    }

    // spec: HOST-LIB-7, HOST-LIB-8
    #[test]
    fn storage_summary_single_game_cache_and_imported_files_avoid_links() {
        let base = std::env::temp_dir().join(format!("hitode-summary-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let data = base.join("data");
        let cache = base.join("cache");
        let external = base.join("external");
        fs::create_dir_all(data.join("imports/copy1")).unwrap();
        fs::create_dir_all(data.join("imports/.staging")).unwrap();
        fs::create_dir_all(data.join("files/file-1")).unwrap();
        fs::create_dir_all(data.join("files/file-2")).unwrap();
        fs::create_dir_all(data.join("wine/drive_c")).unwrap();
        fs::create_dir_all(data.join("wine/dosdevices")).unwrap();
        fs::create_dir_all(data.join("runtime/current")).unwrap();
        fs::create_dir_all(data.join("runtime/old")).unwrap();
        fs::create_dir_all(data.join("covers")).unwrap();
        fs::create_dir_all(cache.join("gpu/g1")).unwrap();
        fs::create_dir_all(cache.join("gpu/g2")).unwrap();
        fs::create_dir_all(&external).unwrap();

        fs::write(external.join("huge.bin"), vec![0u8; 4096]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, data.join("wine/dosdevices/d:")).unwrap();

        fs::write(data.join("imports/copy1/game.exe"), vec![0u8; 100]).unwrap();
        fs::write(data.join("imports/.staging/part"), vec![0u8; 50]).unwrap();
        fs::write(data.join("files/file-1/disc2.iso"), vec![0u8; 200]).unwrap();
        fs::write(data.join("files/file-2/custom.otf"), vec![0u8; 30]).unwrap();
        fs::write(data.join("wine/drive_c/save.dat"), vec![0u8; 40]).unwrap();
        fs::write(data.join("runtime/current/ntdll.dll"), vec![0u8; 60]).unwrap();
        fs::write(data.join("runtime/old/ntdll.dll"), vec![0u8; 70]).unwrap();
        fs::write(data.join("covers/v1-0123456789abcdef.jpg"), vec![0u8; 20]).unwrap();
        fs::write(data.join("library.json"), vec![0u8; 10]).unwrap();
        fs::write(cache.join("gpu/g1/a.native"), vec![0u8; 80]).unwrap();
        fs::write(cache.join("gpu/g2/b.native"), vec![0u8; 90]).unwrap();
        fs::write(cache.join("gpu/vulkan-health.bin"), vec![0u8; 15]).unwrap();

        let summary = measure_storage(&data, &cache, None, Some("current")).unwrap();
        assert_eq!(summary.game_files_bytes, 100 + 200 + 30);
        assert_eq!(summary.env_bytes, 40 + 60 + 20 + 10);
        assert_eq!(summary.cache_bytes, 50 + 70 + 80 + 90 + 15);

        assert_eq!(single_game_cache_bytes(&data, &cache, "g1").unwrap(), 80);
        clear_one_game_cache(&data, &cache, "g1").unwrap();
        assert_eq!(single_game_cache_bytes(&data, &cache, "g1").unwrap(), 0);
        assert_eq!(single_game_cache_bytes(&data, &cache, "g2").unwrap(), 90);
        assert!(cache.join("gpu/vulkan-health.bin").is_file());

        let files = list_files(&data.join("files")).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "disc2.iso");
        assert_eq!(files[0].kind, "disc");
        assert_eq!(files[0].bytes, 200);
        assert_eq!(files[1].name, "custom.otf");
        assert_eq!(files[1].kind, "font");
        delete_file(&data.join("files"), "file-1").unwrap();
        assert_eq!(list_files(&data.join("files")).unwrap().len(), 1);
        assert!(delete_file(&data.join("files"), "../external").is_err());

        fs::remove_dir_all(base).unwrap();
    }

    // spec: HOST-LIB-7
    #[test]
    fn managed_copy_lifecycle_keeps_external_files_and_library_entries() {
        let base = std::env::temp_dir().join(format!("hitode-storage-{}", std::process::id()));
        let base = {
            fs::create_dir(&base).unwrap();
            base.canonicalize().unwrap()
        };
        let app = base.join("app");
        let root = app.join("imports");
        let source = root.join("copy-id");
        let payload = source.join("Collection");
        let external = base.join("external");
        fs::create_dir_all(payload.join("first")).unwrap();
        fs::create_dir_all(payload.join("second")).unwrap();
        fs::write(payload.join("first/game.exe"), b"first").unwrap();
        fs::write(payload.join("second/game.exe"), b"second").unwrap();
        fs::create_dir(root.join(".staging")).unwrap();
        fs::create_dir(&external).unwrap();
        fs::write(external.join("game.exe"), b"outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, root.join("linked-copy")).unwrap();

        let copies = list(&root).unwrap();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].name, "Collection");
        assert_eq!(copies[0].bytes, 11);
        assert!(checked_copy(&root, "../external").is_err());
        assert!(checked_copy(&root, "linked-copy").is_err());

        let mut library = Library::load(app.clone()).unwrap();
        for (id, folder) in [
            ("first", payload.join("first")),
            ("second", payload.join("second")),
            ("outside", external.clone()),
        ] {
            library.games.push(LibraryGame {
                id: id.into(),
                folder,
                exe: "game.exe".into(),
                ..LibraryGame::default()
            });
        }
        let save = app.join("games/first/save.dat");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, b"save").unwrap();
        let destination =
            copy_out(&export_source(&source).unwrap(), &external, "Collection").unwrap();
        assert!(copy_out(&payload, &external, "Collection").is_err());
        assert!(payload.join("first/game.exe").is_file());
        fs::remove_file(destination.join("second/game.exe")).unwrap();
        assert!(use_copy(&mut library, &source, &destination).is_err());
        assert_eq!(library.games[0].folder, payload.join("first"));
        fs::write(destination.join("second/game.exe"), b"second").unwrap();
        use_copy(&mut library, &source, &destination).unwrap();
        let spare = root.join("spare");
        fs::create_dir(&spare).unwrap();
        library.games.push(LibraryGame {
            id: "spare".into(),
            folder: spare.clone(),
            ..LibraryGame::default()
        });
        library.save().unwrap();
        delete_copy(&root, "copy-id").unwrap();
        delete_copy(&root, "spare").unwrap();
        let persisted = Library::load(app).unwrap();
        assert_eq!(persisted.games[0].folder, destination.join("first"));
        assert_eq!(persisted.games[1].folder, destination.join("second"));
        assert_eq!(persisted.games[2].folder, external);
        assert_eq!(persisted.games[3].folder, spare);
        assert!(!spare.exists());
        assert!(save.is_file());
        assert!(external.join("game.exe").is_file());
        fs::remove_dir_all(base).unwrap();
    }
}
