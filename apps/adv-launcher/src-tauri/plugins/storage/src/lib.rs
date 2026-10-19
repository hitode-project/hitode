//! Mobile storage access and transactional folder imports, independent of
//! the generated host projects: SAF on Android, Files on iOS.
#![cfg(any(target_os = "android", target_os = "ios"))]

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::path::PathBuf;
use tauri::{
    AppHandle, Manager, Runtime,
    ipc::Channel,
    plugin::{Builder, PluginHandle, TauriPlugin},
};

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_storage);

type Reply<T> = Result<T, String>;

#[derive(Deserialize, Serialize)]
pub struct StorageStatus {
    pub granted: bool,
}

#[derive(Deserialize, Serialize)]
pub struct Folder {
    pub uri: Option<String>,
    pub path: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
pub struct PickedFile {
    /// The picked file, `None` when the user cancelled. iOS hands over a
    /// copy in its temporary inbox; Android a shared-storage path it can
    /// reach, or else a copy under `dest`.
    pub path: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
pub struct CopiedFolder {
    pub path: PathBuf,
}

pub struct Storage<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> Storage<R> {
    async fn call<T: DeserializeOwned>(&self, command: &str, args: Value) -> Reply<T> {
        self.0
            .run_mobile_plugin_async(command, args)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn pick_folder(&self) -> Reply<Folder> {
        self.call("pickFolder", json!({})).await
    }

    #[cfg(target_os = "ios")]
    pub async fn restore_folders(&self) -> Reply<()> {
        self.call("restoreFolders", json!({})).await
    }

    #[cfg(target_os = "ios")]
    pub async fn prepare_folder(&self, path: &std::path::Path) -> Reply<PathBuf> {
        let folder: CopiedFolder = self.call("prepareFolder", json!({ "path": path })).await?;
        Ok(folder.path)
    }

    /// One file of any type; see `PickedFile` for where it ends up.
    pub async fn pick_file(&self, dest: &std::path::Path) -> Reply<PickedFile> {
        self.call("pickFile", json!({ "dest": dest })).await
    }

    pub async fn copy_folder(
        &self,
        uri: String,
        dest: Option<PathBuf>,
        progress: Option<Channel<Value>>,
        id: Option<String>,
    ) -> Reply<CopiedFolder> {
        let root = self
            .0
            .app()
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("imports");
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let id = match id {
            Some(id) => id,
            None => {
                let mut bytes = [0; 8];
                getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            }
        };
        let dest = dest.unwrap_or_else(|| root.join(&id));
        if dest
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .as_ref()
            != Some(&root)
        {
            return Err("destination must be a private import folder".into());
        }
        self.call(
            "copyFolder",
            json!({ "uri": uri, "id": id, "dest": dest, "progress": progress }),
        )
        .await
    }
}

pub trait StorageExt<R: Runtime> {
    fn storage(&self) -> &Storage<R>;
}

impl<R: Runtime, T: Manager<R>> StorageExt<R> for T {
    fn storage(&self) -> &Storage<R> {
        self.state::<Storage<R>>().inner()
    }
}

#[tauri::command]
async fn storage_status<R: Runtime>(app: AppHandle<R>) -> Reply<StorageStatus> {
    app.storage().call("storageStatus", json!({})).await
}

#[tauri::command]
async fn request_storage<R: Runtime>(app: AppHandle<R>) -> Reply<StorageStatus> {
    app.storage().call("requestStorage", json!({})).await
}

#[tauri::command]
async fn pick_folder<R: Runtime>(app: AppHandle<R>) -> Reply<Folder> {
    app.storage().pick_folder().await
}

#[tauri::command]
async fn folder_size<R: Runtime>(app: AppHandle<R>, uri: String) -> Reply<u64> {
    app.storage()
        .call("folderSize", json!({ "uri": uri }))
        .await
}

#[tauri::command]
async fn copy_folder<R: Runtime>(
    app: AppHandle<R>,
    uri: String,
    dest: Option<PathBuf>,
    progress: Channel<Value>,
    id: Option<String>,
) -> Reply<CopiedFolder> {
    app.storage()
        .copy_folder(uri, dest, Some(progress), id)
        .await
}

#[tauri::command]
async fn cancel_copy<R: Runtime>(app: AppHandle<R>, id: String) -> Reply<()> {
    app.storage().call("cancelCopy", json!({ "id": id })).await
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("storage")
        .invoke_handler(tauri::generate_handler![
            storage_status,
            request_storage,
            pick_folder,
            folder_size,
            copy_folder,
            cancel_copy
        ])
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            let handle = api.register_android_plugin("org.hitode.storage", "StoragePlugin")?;
            #[cfg(target_os = "ios")]
            let handle = api.register_ios_plugin(init_plugin_storage)?;
            app.manage(Storage(handle));
            Ok(())
        })
        .build()
}
