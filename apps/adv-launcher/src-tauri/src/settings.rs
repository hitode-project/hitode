//! Launcher file selection and native font discovery.
use std::path::PathBuf;
#[cfg(desktop)]
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
#[cfg(desktop)]
pub async fn pick_game_file(app: tauri::AppHandle, kind: String) -> Result<Option<String>, String> {
    let picker = app.dialog().file();
    let picker = match kind.as_str() {
        "disc" => picker.add_filter(
            "Disc image",
            &["iso", "cue", "bin", "img", "ccd", "mds", "mdf", "nrg"],
        ),
        "font" => picker.add_filter("Font", &["ttf", "ttc", "otf", "otc"]),
        "registry" => picker.add_filter("Registry file", &["reg", "bat", "cmd"]),
        "run" => picker.add_filter(
            "Executable, script or registry",
            &["exe", "bat", "cmd", "reg"],
        ),
        "component" => picker.add_filter("Runtime installer or DLL", &["exe", "dll", "cab"]),
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
/// Direct shared-storage paths on Android can also open descriptors (`.mds`,
/// `.cue`, `.ccd`) beside their data files; single-file copies only accept
/// self-contained images. Copies live beside the library, out of the iOS
/// picker's temporary inbox.
#[tauri::command]
#[cfg(mobile)]
pub async fn pick_game_file(app: tauri::AppHandle, kind: String) -> Result<Option<String>, String> {
    use tauri::Manager;
    use tauri_plugin_storage::StorageExt;
    let (direct_extensions, copy_extensions): (&[&str], &[&str]) = match kind.as_str() {
        "disc" => (
            &["iso", "cue", "bin", "img", "ccd", "mds", "mdf", "nrg"],
            &["iso", "bin", "img", "mdf", "nrg"],
        ),
        "font" => (&["ttf", "ttc", "otf", "otc"], &["ttf", "ttc", "otf", "otc"]),
        "registry" => (&["reg", "bat", "cmd"], &["reg", "bat", "cmd"]),
        "run" => (&["exe", "bat", "cmd", "reg"], &["exe", "bat", "cmd", "reg"]),
        "component" => (&["exe", "dll", "cab"], &["exe", "dll", "cab"]),
        _ => return Err("unknown file type".into()),
    };
    let files = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("files");
    std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
    #[cfg(target_os = "ios")]
    crate::game_storage::exclude_from_backup(&files);
    let Some(mut path) = app
        .storage()
        .pick_file_filtered(&files, direct_extensions, copy_extensions)
        .await?
        .path
    else {
        return Ok(None);
    };
    let allowed = if cfg!(target_os = "ios") || is_private_copy(&files, &path) {
        copy_extensions
    } else {
        direct_extensions
    };
    let matches = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| allowed.contains(&e.to_ascii_lowercase().as_str()));
    if !matches {
        cleanup_rejected_pick(&files, &path, cfg!(target_os = "ios"));
        return Err(format!("choose a .{} file", allowed.join(" / .")));
    }
    if cfg!(target_os = "ios") {
        let folder = fresh_folder(&files)?;
        let target = folder.join(path.file_name().ok_or("the file has no name")?);
        std::fs::rename(&path, &target).map_err(|e| e.to_string())?;
        path = target;
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[cfg(any(mobile, test))]
fn is_private_copy(files_dir: &std::path::Path, path: &std::path::Path) -> bool {
    files_dir
        .canonicalize()
        .ok()
        .zip(path.canonicalize().ok())
        .is_some_and(|(dir, target)| target.starts_with(dir))
}

/// Removes a private copy created by the mobile picker when validation rejects it,
/// without touching files that live outside `<app_data_dir>/files` on Android.
#[cfg(any(mobile, test))]
pub(crate) fn cleanup_rejected_pick(
    files_dir: &std::path::Path,
    path: &std::path::Path,
    remove_inbox: bool,
) {
    if let Ok(files_dir) = files_dir.canonicalize()
        && let Ok(canonical) = path.canonicalize()
        && let Ok(relative) = canonical.strip_prefix(&files_dir)
    {
        if let Some(std::path::Component::Normal(first)) = relative.components().next() {
            let entry = files_dir.join(first);
            if let Ok(meta) = std::fs::symlink_metadata(&entry) {
                if meta.is_dir() {
                    let _ = std::fs::remove_dir_all(entry);
                } else if meta.is_file() {
                    let _ = std::fs::remove_file(entry);
                }
            }
        }
        return;
    }
    if remove_inbox && std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()) {
        let _ = std::fs::remove_file(path);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_pick_cleanup_removes_private_copy_and_keeps_external_files() {
        let base = std::env::temp_dir().join(format!("hitode-pick-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let files = base.join("files");
        let copy_dir = files.join("file-123");
        let copy = copy_dir.join("wrong.txt");
        let external = base.join("external.txt");
        let inbox = base.join("inbox.txt");
        std::fs::create_dir_all(&copy_dir).unwrap();
        std::fs::write(&copy, b"copy").unwrap();
        std::fs::write(&external, b"external").unwrap();
        std::fs::write(&inbox, b"inbox").unwrap();

        cleanup_rejected_pick(&files, &copy, false);
        assert!(!copy_dir.exists());

        assert!(is_private_copy(&files, &files));
        assert!(!is_private_copy(&files, &external));

        cleanup_rejected_pick(&files, &external, false);
        assert!(external.is_file());

        cleanup_rejected_pick(&files, &inbox, true);
        assert!(!inbox.exists());

        std::fs::remove_dir_all(base).unwrap();
    }
}
