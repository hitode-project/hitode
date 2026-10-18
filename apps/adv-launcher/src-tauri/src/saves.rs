//! Per-game data snapshots include guest writes and registry state, never original game files.
use crate::library::Library;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{Emitter, Manager, State, WebviewWindow};

static BUSY: AtomicBool = AtomicBool::new(false);
pub(crate) fn busy() -> bool {
    BUSY.load(Ordering::Acquire)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    auto_backup: bool,
    keep: usize,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            auto_backup: true,
            keep: 5,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    id: String,
    when: u64,
    kind: String,
    bytes: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    path: PathBuf,
    backups: Vec<Backup>,
    auto_backup: bool,
    keep: usize,
}
fn root(data: &Path) -> PathBuf {
    data.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("backups")
        .join(data.file_name().unwrap())
}
fn policy(data: &Path) -> io::Result<Policy> {
    match fs::read(root(data).join("policy.json")) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Policy::default()),
        Err(e) => Err(e),
    }
}
fn info(data: &Path) -> io::Result<Info> {
    let policy = policy(data)?;
    let mut backups = Vec::new();
    if root(data).exists() {
        for entry in fs::read_dir(root(data))? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() || entry.file_name().to_string_lossy().starts_with('.')
            {
                continue;
            }
            let manifest = entry.path().join("snapshot.json");
            if manifest.is_file() {
                backups.push(serde_json::from_slice::<Backup>(&fs::read(manifest)?)?);
            }
        }
    }
    backups.sort_by(|a, b| b.when.cmp(&a.when).then_with(|| b.id.cmp(&a.id)));
    Ok(Info {
        path: data.to_owned(),
        backups,
        auto_backup: policy.auto_backup,
        keep: policy.keep,
    })
}
fn copy_tree(source: &Path, target: &Path) -> io::Result<u64> {
    fs::create_dir_all(target)?;
    let mut bytes = 0;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let to = target.join(entry.file_name());
        if kind.is_dir() {
            bytes += copy_tree(&entry.path(), &to)?;
        } else if kind.is_file() {
            bytes += fs::copy(entry.path(), to)?;
        } else {
            return Err(io::Error::other(
                "snapshots cannot contain symbolic links or special files",
            ));
        }
    }
    Ok(bytes)
}
fn identity() -> io::Result<String> {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(format!(
        "{}-{:016x}",
        crate::library::now(),
        u64::from_le_bytes(bytes)
    ))
}
fn snapshot(data: &Path, destination: &Path, kind: &str) -> io::Result<Backup> {
    fs::create_dir_all(destination)?;
    if destination
        .canonicalize()?
        .starts_with(data.canonicalize()?)
    {
        return Err(io::Error::other(
            "export destination cannot be inside the game data folder",
        ));
    }
    let id = identity()?;
    let temp = destination.join(format!(".{id}"));
    let bytes = copy_tree(data, &temp.join("data"))?;
    let backup = Backup {
        id: id.clone(),
        when: crate::library::now(),
        kind: kind.into(),
        bytes,
    };
    fs::write(temp.join("snapshot.json"), serde_json::to_vec(&backup)?)?;
    fs::rename(temp, destination.join(id))?;
    Ok(backup)
}
fn restore(data: &Path, source: &Path) -> io::Result<()> {
    let _: Backup = serde_json::from_slice(&fs::read(source.join("snapshot.json"))?)?;
    let token = identity()?;
    let parent = data.parent().unwrap();
    let temp = parent.join(format!("restore-{token}"));
    copy_tree(&source.join("data"), &temp)?;
    let old = parent.join(format!("previous-{token}"));
    let exists = data.exists();
    if exists {
        fs::rename(data, &old)?;
    }
    if let Err(e) = fs::rename(&temp, data) {
        if exists {
            fs::rename(&old, data)?;
        }
        return Err(e);
    }
    if exists {
        fs::remove_dir_all(old)?;
    }
    Ok(())
}
pub(crate) fn automatic(data: &Path) -> io::Result<()> {
    let config = policy(data)?;
    if !config.auto_backup || !data.is_dir() {
        return Ok(());
    }
    snapshot(data, &root(data), "automatic")?;
    let mut kept = 0;
    for backup in info(data)?.backups {
        if backup.kind != "automatic" {
            continue;
        }
        kept += 1;
        if kept > config.keep {
            fs::remove_dir_all(root(data).join(backup.id))?;
        }
    }
    Ok(())
}
/// The automatic backup after a game exits; a failure is reported to the
/// page as `backup_failed`.
pub(crate) fn after_game(window: &WebviewWindow, data: &Path) {
    if let Err(error) = automatic(data) {
        let _ = window.emit("backup_failed", error.to_string());
    }
}
#[tauri::command]
pub fn save_info(state: State<'_, Mutex<Library>>, id: String) -> Result<Info, String> {
    let data = state
        .lock()
        .unwrap()
        .data_dir(&id)
        .map_err(|e| e.to_string())?;
    info(&data).map_err(|e| e.to_string())
}
#[tauri::command]
pub fn save_policy(
    state: State<'_, Mutex<Library>>,
    id: String,
    policy: Policy,
) -> Result<(), String> {
    if ![3, 5, 10].contains(&policy.keep) {
        return Err("invalid backup retention".into());
    }
    let data = state
        .lock()
        .unwrap()
        .data_dir(&id)
        .map_err(|e| e.to_string())?;
    fs::create_dir_all(root(&data)).map_err(|e| e.to_string())?;
    fs::write(
        root(&data).join("policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn save_action(
    window: WebviewWindow,
    id: String,
    action: String,
    backup: Option<String>,
    folder: Option<PathBuf>,
) -> Result<Info, String> {
    let data = window
        .state::<Mutex<Library>>()
        .lock()
        .unwrap()
        .data_dir(&id)
        .map_err(|e| e.to_string())?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let result = if crate::game::is_running() {
                Err("请先退出游戏，再操作存档".to_string())
            } else if BUSY.swap(true, Ordering::AcqRel) {
                Err("存档操作正在进行".to_string())
            } else {
                Ok(())
            };
            let _ = send.send(result);
        })
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        receive.recv().map_err(|e| e.to_string())??;
        struct Busy;
        impl Drop for Busy {
            fn drop(&mut self) {
                BUSY.store(false, Ordering::Release);
            }
        }
        let _busy = Busy;
        let operation = || -> io::Result<Info> {
            match action.as_str() {
                "backup" => {
                    snapshot(&data, &root(&data), "manual")?;
                }
                "restore" => {
                    let id = backup.ok_or_else(|| io::Error::other("missing backup"))?;
                    if Path::new(&id).file_name().and_then(|v| v.to_str()) != Some(&id) {
                        return Err(io::Error::other("invalid backup"));
                    }
                    restore(&data, &root(&data).join(id))?;
                }
                "export" => {
                    snapshot(
                        &data,
                        &folder.ok_or_else(|| io::Error::other("missing destination"))?,
                        "manual",
                    )?;
                }
                "import" => {
                    restore(
                        &data,
                        &folder.ok_or_else(|| io::Error::other("missing snapshot folder"))?,
                    )?;
                }
                _ => return Err(io::Error::other("unknown save action")),
            }
            info(&data)
        };
        operation().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("adv-backups-{}", identity().unwrap()));
        let data = dir.join("games/test");
        fs::create_dir_all(data.join("nested")).unwrap();
        fs::write(data.join("registry.json"), b"registry state").unwrap();
        fs::write(data.join("nested/save.bin"), [0, 255, 42]).unwrap();
        (dir, data)
    }
    // spec: HOST-LIB-5
    #[test]
    fn backup_export_import_round_trip_includes_registry_and_guest_files() {
        let (dir, data) = fixture();
        let backup = snapshot(&data, &root(&data), "manual").unwrap();
        assert_eq!(backup.bytes, 17);
        let exported = snapshot(&data, &dir.join("export"), "manual").unwrap();
        fs::write(data.join("registry.json"), b"changed").unwrap();
        fs::write(data.join("extra"), b"remove me").unwrap();
        restore(&data, &dir.join("export").join(exported.id)).unwrap();
        assert_eq!(
            fs::read(data.join("registry.json")).unwrap(),
            b"registry state"
        );
        assert_eq!(
            fs::read(data.join("nested/save.bin")).unwrap(),
            [0, 255, 42]
        );
        assert!(!data.join("extra").exists());
        assert_eq!(info(&data).unwrap().backups.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    // spec: HOST-LIB-5
    #[test]
    fn invalid_import_keeps_current_data_and_export_cannot_recurse() {
        let (dir, data) = fixture();
        let backup = snapshot(&data, &root(&data), "manual").unwrap();
        let source = root(&data).join(backup.id);
        std::os::unix::fs::symlink(&data, source.join("data/link")).unwrap();
        assert!(restore(&data, &source).is_err());
        assert_eq!(
            fs::read(data.join("registry.json")).unwrap(),
            b"registry state"
        );
        assert!(snapshot(&data, &data.join("export"), "manual").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    // spec: HOST-LIB-5
    #[test]
    fn automatic_retention_preserves_manual_backups_and_obeys_disabled_policy() {
        let (dir, data) = fixture();
        let manual = snapshot(&data, &root(&data), "manual").unwrap();
        fs::write(
            root(&data).join("policy.json"),
            br#"{"autoBackup":true,"keep":3}"#,
        )
        .unwrap();
        for _ in 0..5 {
            automatic(&data).unwrap();
        }
        let backups = info(&data).unwrap().backups;
        assert_eq!(backups.len(), 4);
        assert!(backups.iter().any(|b| b.id == manual.id));
        fs::write(
            root(&data).join("policy.json"),
            br#"{"autoBackup":false,"keep":3}"#,
        )
        .unwrap();
        automatic(&data).unwrap();
        assert_eq!(info(&data).unwrap().backups.len(), 4);
        fs::remove_dir_all(dir).unwrap();
    }
}
