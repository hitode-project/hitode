//! The player's Wine prefix, which every game shares as one Windows user's
//! machine. Games write their own folders in place; what they write
//! elsewhere lands on its drive C:, which the player opens from the launcher.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io;
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

const PENDING_RESET_FILE: &str = "wine-reset.json";
const REGISTRY_HIVES: &[&str] = &["system.reg", "user.reg", "userdef.reg"];
const PROGRAM_DIRS: &[&str] = &["Program Files", "Program Files (x86)", "ProgramData"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WineCleanupTarget {
    Temp,
    Registry,
    Windows,
    Programs,
    Users,
}

impl WineCleanupTarget {
    const ALL: [Self; 5] = [
        Self::Temp,
        Self::Registry,
        Self::Windows,
        Self::Programs,
        Self::Users,
    ];

    fn immediate_while_running(self) -> bool {
        matches!(self, Self::Temp)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WineEnvironmentStatus {
    pub total_bytes: u64,
    pub temp_bytes: u64,
    pub registry_bytes: u64,
    pub windows_bytes: u64,
    pub programs_bytes: u64,
    pub users_bytes: u64,
    pub server_started: bool,
    pub pending_reset: Vec<WineCleanupTarget>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WineCleanupResult {
    pub executed: Vec<WineCleanupTarget>,
    pub scheduled: Vec<WineCleanupTarget>,
    pub freed_bytes: u64,
    pub status: WineEnvironmentStatus,
}

fn canonical_targets(
    targets: impl IntoIterator<Item = WineCleanupTarget>,
) -> Vec<WineCleanupTarget> {
    let set: HashSet<WineCleanupTarget> = targets.into_iter().collect();
    WineCleanupTarget::ALL
        .into_iter()
        .filter(|target| set.contains(target))
        .collect()
}

fn real_directory(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.is_dir()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn real_file_bytes(path: &Path) -> io::Result<u64> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(meta.len()),
        Ok(_) => Ok(0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

/// Walks a real directory tree without following symbolic links.
fn real_tree_bytes(path: &Path) -> io::Result<u64> {
    if !real_directory(path)? {
        return Ok(0);
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                total = total.saturating_add(entry.metadata()?.len());
            }
        }
    }
    Ok(total)
}

/// Finds real subdirectories of `parent` matching any name in `names` case-insensitively.
fn find_ci_dirs(parent: &Path, names: &[&str]) -> io::Result<Vec<PathBuf>> {
    if !real_directory(parent)? {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() || !kind.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if names
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
        {
            found.push(entry.path());
        }
    }
    Ok(found)
}

fn windows_temp_dirs(drive: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for windows in find_ci_dirs(drive, &["windows"])? {
        out.extend(find_ci_dirs(&windows, &["temp"])?);
    }
    Ok(out)
}

fn user_temp_dirs(drive: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for users in find_ci_dirs(drive, &["users"])? {
        for user in fs::read_dir(&users)? {
            let user = user?;
            let kind = user.file_type()?;
            if kind.is_symlink() || !kind.is_dir() {
                continue;
            }
            let user_dir = user.path();
            out.extend(find_ci_dirs(&user_dir, &["temp"])?);
            for appdata in find_ci_dirs(&user_dir, &["appdata"])? {
                for local in find_ci_dirs(&appdata, &["local"])? {
                    out.extend(find_ci_dirs(&local, &["temp"])?);
                }
            }
        }
    }
    Ok(out)
}

fn mount_temp_dirs(prefix: &Path) -> io::Result<Vec<PathBuf>> {
    if !real_directory(prefix)? {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(prefix)? {
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
            out.push(entry.path());
        }
    }
    Ok(out)
}

fn pending_reset_path(data_dir: &Path) -> PathBuf {
    data_dir.join(PENDING_RESET_FILE)
}

fn read_pending_reset(data_dir: &Path) -> Vec<WineCleanupTarget> {
    let path = pending_reset_path(data_dir);
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let Ok(targets) = serde_json::from_slice::<Vec<WineCleanupTarget>>(&bytes) else {
        return Vec::new();
    };
    canonical_targets(targets)
}

fn write_pending_reset(data_dir: &Path, targets: &[WineCleanupTarget]) -> io::Result<()> {
    let path = pending_reset_path(data_dir);
    if targets.is_empty() {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
    }
    fs::create_dir_all(data_dir)?;
    let tmp = data_dir.join(format!("{PENDING_RESET_FILE}.tmp"));
    let json = serde_json::to_vec(targets).map_err(io::Error::other)?;
    fs::write(&tmp, json)?;
    fs::rename(tmp, path)
}

fn clear_dir_contents(dir: &Path) -> io::Result<()> {
    if !real_directory(dir)? {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_symlink() || !kind.is_dir() {
            fs::remove_file(path)?;
        } else {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

fn remove_real_dir(dir: &Path) -> io::Result<()> {
    if real_directory(dir)? {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

fn remove_real_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sum_dirs(paths: &[PathBuf]) -> io::Result<u64> {
    let mut total = 0u64;
    for path in paths {
        total = total.saturating_add(real_tree_bytes(path)?);
    }
    Ok(total)
}

fn measure_wine_environment(
    data_dir: &Path,
    prefix: &Path,
    drive: &Path,
    server_started: bool,
) -> io::Result<WineEnvironmentStatus> {
    let win_temps = windows_temp_dirs(drive)?;
    let user_temps = user_temp_dirs(drive)?;
    let mount_temps = mount_temp_dirs(prefix)?;
    let win_temp_bytes = sum_dirs(&win_temps)?;
    let user_temp_bytes = sum_dirs(&user_temps)?;
    let mount_temp_bytes = sum_dirs(&mount_temps)?;
    let temp_bytes = win_temp_bytes
        .saturating_add(user_temp_bytes)
        .saturating_add(mount_temp_bytes);

    let mut registry_bytes = 0u64;
    for hive in REGISTRY_HIVES {
        registry_bytes = registry_bytes.saturating_add(real_file_bytes(&prefix.join(hive))?);
    }

    let windows_total = sum_dirs(&find_ci_dirs(drive, &["windows"])?)?;
    let windows_bytes = windows_total.saturating_sub(win_temp_bytes);

    let programs_bytes = sum_dirs(&find_ci_dirs(drive, PROGRAM_DIRS)?)?;

    let users_total = sum_dirs(&find_ci_dirs(drive, &["users"])?)?;
    let users_bytes = users_total.saturating_sub(user_temp_bytes);

    let mut total_bytes = real_tree_bytes(prefix)?;
    if !drive.starts_with(prefix) {
        total_bytes = total_bytes.saturating_add(real_tree_bytes(drive)?);
    }

    Ok(WineEnvironmentStatus {
        total_bytes,
        temp_bytes,
        registry_bytes,
        windows_bytes,
        programs_bytes,
        users_bytes,
        server_started,
        pending_reset: read_pending_reset(data_dir),
    })
}

fn execute_wine_targets(
    prefix: &Path,
    drive: &Path,
    targets: &[WineCleanupTarget],
) -> io::Result<()> {
    for &target in targets {
        match target {
            WineCleanupTarget::Temp => {
                for dir in windows_temp_dirs(drive)? {
                    clear_dir_contents(&dir)?;
                }
                for dir in user_temp_dirs(drive)? {
                    clear_dir_contents(&dir)?;
                }
                for dir in mount_temp_dirs(prefix)? {
                    remove_real_dir(&dir)?;
                }
            }
            WineCleanupTarget::Registry => {
                for hive in REGISTRY_HIVES {
                    remove_real_file(&prefix.join(hive))?;
                }
                remove_real_dir(&prefix.join("server"))?;
            }
            WineCleanupTarget::Windows => {
                for dir in find_ci_dirs(drive, &["windows"])? {
                    remove_real_dir(&dir)?;
                }
            }
            WineCleanupTarget::Programs => {
                for dir in find_ci_dirs(drive, PROGRAM_DIRS)? {
                    remove_real_dir(&dir)?;
                }
            }
            WineCleanupTarget::Users => {
                for dir in find_ci_dirs(drive, &["users"])? {
                    remove_real_dir(&dir)?;
                }
            }
        }
    }
    Ok(())
}

fn clean_wine_in(
    data_dir: &Path,
    prefix: &Path,
    drive: &Path,
    targets: Vec<WineCleanupTarget>,
    server_started: bool,
) -> io::Result<WineCleanupResult> {
    let chosen = canonical_targets(targets);
    let before = measure_wine_environment(data_dir, prefix, drive, server_started)?;
    let mut immediate = Vec::new();
    let mut deferred = Vec::new();
    for target in chosen {
        if !server_started || target.immediate_while_running() {
            immediate.push(target);
        } else {
            deferred.push(target);
        }
    }
    if !immediate.is_empty() {
        execute_wine_targets(prefix, drive, &immediate)?;
    }
    if !deferred.is_empty() {
        let mut merged = read_pending_reset(data_dir);
        merged.extend(deferred.iter().copied());
        let merged = canonical_targets(merged);
        write_pending_reset(data_dir, &merged)?;
    }
    let after = measure_wine_environment(data_dir, prefix, drive, server_started)?;
    let freed_bytes = before.total_bytes.saturating_sub(after.total_bytes);
    Ok(WineCleanupResult {
        executed: immediate,
        scheduled: deferred,
        freed_bytes,
        status: after,
    })
}

/// Runs any scheduled Wine environment reset before the execution owner starts.
pub(crate) fn apply_pending_reset(app: &AppHandle) {
    let Ok(data_dir) = app.path().app_data_dir() else {
        return;
    };
    let Ok(prefix) = prefix(app) else {
        return;
    };
    let Ok(drive) = drive(app) else {
        return;
    };
    let pending = read_pending_reset(&data_dir);
    if pending.is_empty() {
        return;
    }
    let _ = execute_wine_targets(&prefix, &drive, &pending);
    let _ = write_pending_reset(&data_dir, &[]);
}

#[tauri::command]
pub async fn wine_environment_status(app: AppHandle) -> Result<WineEnvironmentStatus, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let prefix = prefix(&app).map_err(|e| e.to_string())?;
    let drive = drive(&app).map_err(|e| e.to_string())?;
    let server_started = adv_shell::wine_server_started();
    tauri::async_runtime::spawn_blocking(move || {
        measure_wine_environment(&data_dir, &prefix, &drive, server_started)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn clean_wine_environment(
    app: AppHandle,
    targets: Vec<WineCleanupTarget>,
) -> Result<WineCleanupResult, String> {
    let files = app.state::<crate::game_storage::Activity>().acquire()?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let prefix = prefix(&app).map_err(|e| e.to_string())?;
    let drive = drive(&app).map_err(|e| e.to_string())?;
    let server_started = adv_shell::wine_server_started();
    tauri::async_runtime::spawn_blocking(move || {
        let _files = files;
        let result = clean_wine_in(&data_dir, &prefix, &drive, targets, server_started)
            .map_err(|e| e.to_string())?;
        if server_started && !result.executed.is_empty() {
            adv_shell::wine_names_changed();
        }
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cancel_wine_reset(app: AppHandle) -> Result<WineEnvironmentStatus, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let prefix = prefix(&app).map_err(|e| e.to_string())?;
    let drive = drive(&app).map_err(|e| e.to_string())?;
    let server_started = adv_shell::wine_server_started();
    tauri::async_runtime::spawn_blocking(move || {
        write_pending_reset(&data_dir, &[]).map_err(|e| e.to_string())?;
        measure_wine_environment(&data_dir, &prefix, &drive, server_started)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub fn restart_launcher(app: AppHandle) {
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("hitode-drive-{label}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed_prefix(prefix: &Path, drive: &Path) {
        fs::create_dir_all(prefix.join("server")).unwrap();
        fs::create_dir_all(prefix.join(".hitode-files-12")).unwrap();
        fs::create_dir_all(drive.join("windows/system32")).unwrap();
        fs::create_dir_all(drive.join("windows/Temp")).unwrap();
        fs::create_dir_all(drive.join("Program Files/Demo")).unwrap();
        fs::create_dir_all(drive.join("ProgramData/Vendor")).unwrap();
        fs::create_dir_all(drive.join("users/Player/AppData/Local/Temp")).unwrap();
        fs::create_dir_all(drive.join("users/Player/AppData/Roaming/Save")).unwrap();
        fs::create_dir_all(drive.join("users/Player/Temp")).unwrap();

        fs::write(prefix.join("system.reg"), vec![0u8; 100]).unwrap();
        fs::write(prefix.join("user.reg"), vec![0u8; 50]).unwrap();
        fs::write(prefix.join("userdef.reg"), vec![0u8; 25]).unwrap();
        fs::write(prefix.join("server/socket"), vec![0u8; 10]).unwrap();
        fs::write(prefix.join(".hitode-files-12/tmp"), vec![0u8; 30]).unwrap();
        fs::write(drive.join("windows/system32/mfc140u.dll"), vec![0u8; 200]).unwrap();
        fs::write(drive.join("windows/Temp/setup.tmp"), vec![0u8; 40]).unwrap();
        fs::write(drive.join("Program Files/Demo/app.bin"), vec![0u8; 300]).unwrap();
        fs::write(drive.join("ProgramData/Vendor/cfg.dat"), vec![0u8; 60]).unwrap();
        fs::write(
            drive.join("users/Player/AppData/Local/Temp/cache.tmp"),
            vec![0u8; 70],
        )
        .unwrap();
        fs::write(drive.join("users/Player/Temp/old.tmp"), vec![0u8; 20]).unwrap();
        fs::write(
            drive.join("users/Player/AppData/Roaming/Save/slot1.sav"),
            vec![0u8; 80],
        )
        .unwrap();
    }

    // spec: HOST-LIB-12
    #[test]
    fn measure_and_clean_wine_environment_without_double_counting_or_following_symlinks() {
        let root = unique_temp("measure-clean");
        let data = root.join("data");
        let prefix = data.join("wine");
        let drive = prefix.join("drive_c");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("external.bin"), vec![0u8; 999]).unwrap();
        seed_prefix(&prefix, &drive);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside.join("external.bin"),
                drive.join("windows/system32/wine_builtin.dll"),
            )
            .unwrap();
            std::os::unix::fs::symlink(&outside, drive.join("windows/Temp/link")).unwrap();
        }

        let status = measure_wine_environment(&data, &prefix, &drive, false).unwrap();
        assert_eq!(status.temp_bytes, 30 + 40 + 70 + 20);
        assert_eq!(status.registry_bytes, 100 + 50 + 25);
        assert_eq!(status.windows_bytes, 200);
        assert_eq!(status.programs_bytes, 300 + 60);
        assert_eq!(status.users_bytes, 80);
        assert_eq!(
            status.total_bytes,
            status.temp_bytes
                + status.registry_bytes
                + status.windows_bytes
                + status.programs_bytes
                + status.users_bytes
                + 10
        );

        let cleaned = clean_wine_in(
            &data,
            &prefix,
            &drive,
            vec![
                WineCleanupTarget::Temp,
                WineCleanupTarget::Registry,
                WineCleanupTarget::Windows,
            ],
            false,
        )
        .unwrap();
        assert_eq!(
            cleaned.executed,
            vec![
                WineCleanupTarget::Temp,
                WineCleanupTarget::Registry,
                WineCleanupTarget::Windows,
            ]
        );
        assert!(cleaned.scheduled.is_empty());
        assert_eq!(cleaned.status.temp_bytes, 0);
        assert_eq!(cleaned.status.registry_bytes, 0);
        assert_eq!(cleaned.status.windows_bytes, 0);
        assert_eq!(cleaned.status.programs_bytes, 360);
        assert_eq!(cleaned.status.users_bytes, 80);
        assert!(
            drive
                .join("users/Player/AppData/Roaming/Save/slot1.sav")
                .is_file()
        );
        assert!(outside.join("external.bin").is_file());
        let _ = fs::remove_dir_all(root);
    }

    // spec: HOST-LIB-12
    #[test]
    fn running_server_executes_temp_immediately_and_schedules_rest_for_next_launch() {
        let root = unique_temp("deferred-reset");
        let data = root.join("data");
        let prefix = data.join("wine");
        let drive = root.join("Documents/C");
        seed_prefix(&prefix, &drive);

        let result = clean_wine_in(
            &data,
            &prefix,
            &drive,
            vec![
                WineCleanupTarget::Temp,
                WineCleanupTarget::Registry,
                WineCleanupTarget::Programs,
            ],
            true,
        )
        .unwrap();
        assert_eq!(result.executed, vec![WineCleanupTarget::Temp]);
        assert_eq!(
            result.scheduled,
            vec![WineCleanupTarget::Registry, WineCleanupTarget::Programs]
        );
        assert_eq!(result.status.temp_bytes, 0);
        assert_eq!(result.status.registry_bytes, 175);
        assert_eq!(result.status.programs_bytes, 360);
        assert_eq!(
            result.status.pending_reset,
            vec![WineCleanupTarget::Registry, WineCleanupTarget::Programs]
        );

        // Simulate next startup applying pending reset before server starts.
        let pending = read_pending_reset(&data);
        execute_wine_targets(&prefix, &drive, &pending).unwrap();
        write_pending_reset(&data, &[]).unwrap();

        let after = measure_wine_environment(&data, &prefix, &drive, false).unwrap();
        assert_eq!(after.registry_bytes, 0);
        assert_eq!(after.programs_bytes, 0);
        assert_eq!(after.users_bytes, 80);
        assert!(after.pending_reset.is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
