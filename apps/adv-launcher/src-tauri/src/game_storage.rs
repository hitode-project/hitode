//! Managed game copies and rebuildable caches (HOST-LIB-7, HOST-LIB-8).
use crate::library::Library;
use serde::Serialize;
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

fn real_directory(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.is_dir()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn clear_gpu_caches(games: &Path) -> io::Result<()> {
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

#[tauri::command]
pub(crate) async fn clear_game_cache(app: AppHandle) -> Reply<()> {
    let files = app.state::<Activity>().acquire()?;
    let games = app.path().app_data_dir().map_err(error)?.join("games");
    worker(move || {
        let _files = files;
        clear_gpu_caches(&games)
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
    })
}

#[tauri::command]
pub(crate) async fn game_file_location(app: AppHandle, game_id: String) -> Reply<Location> {
    let folder = game_folder(&app, &game_id)?;
    let mut location = location(&app, &folder)?;
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
        fs::create_dir(&base).unwrap();
        let games = base.join("games");
        let artifacts = games.join("first/artifacts");
        let gpu = artifacts.join("gpu");
        let outside = base.join("outside");
        fs::create_dir_all(&gpu).unwrap();
        fs::create_dir_all(outside.join("gpu")).unwrap();
        let save = games.join("first/save.dat");
        fs::write(&save, b"save").unwrap();
        fs::write(artifacts.join("trace"), b"trace").unwrap();
        fs::write(gpu.join("shader.wgsl-cache"), b"cache").unwrap();
        #[cfg(unix)]
        {
            fs::create_dir(games.join("linked")).unwrap();
            std::os::unix::fs::symlink(&outside, games.join("linked/artifacts")).unwrap();
            std::os::unix::fs::symlink(&outside, gpu.join("link")).unwrap();
        }

        clear_gpu_caches(&games).unwrap();
        clear_gpu_caches(&games).unwrap();
        assert!(!gpu.exists());
        assert_eq!(fs::read(save).unwrap(), b"save");
        assert!(artifacts.join("trace").is_file());
        assert!(outside.join("gpu").is_dir());
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
