//! Game-run types and bookkeeping shared by the macOS and Android hosts.
use crate::library::{self, LastRun, Library};
use adv_shell::Session;
use serde::Serialize;
use std::{sync::Mutex, time::Duration};
use tauri::{Emitter, Manager, WebviewWindow};

#[derive(Clone, Serialize)]
#[serde(untagged)]
enum Finished {
    Exit {
        #[serde(rename = "gameId")]
        game_id: String,
        #[serde(rename = "exitCode")]
        exit_code: u8,
        /// Failure and frame timing for the anonymous report.
        summary: adv_report::RunSummary,
    },
    Error {
        #[serde(rename = "gameId")]
        game_id: String,
        error: String,
        /// The failure kind and time bucket; never the error text.
        summary: adv_report::RunSummary,
    },
}

/// Records the play session in the library and emits `game_finished`.
pub(crate) fn finished(
    window: &WebviewWindow,
    id: String,
    result: Result<adv_shell::Finished, String>,
    elapsed: Duration,
) {
    let summary = match &result {
        Ok(finished) => finished.summary.clone(),
        Err(error) => adv_shell::error_summary(error, elapsed),
    };
    if let Err(error) = window.state::<Mutex<Library>>().lock().unwrap().finished(
        &id,
        elapsed.as_secs() as u32,
        LastRun {
            ended: library::now(),
            summary: Some(summary.clone()),
            error: result.as_ref().err().cloned(),
        },
    ) {
        eprintln!("could not save play session: {error}");
    }
    let payload = match result {
        Ok(finished) => Finished::Exit {
            game_id: id,
            exit_code: finished.exit_code,
            summary,
        },
        Err(error) => Finished::Error {
            game_id: id,
            summary,
            error,
        },
    };
    let _ = window.emit("game_finished", payload);
}

pub(crate) fn upscaler(name: &str) -> Result<adv_shell::Upscaler, String> {
    use adv_shell::Upscaler;
    match name {
        "off" => Ok(Upscaler::Off),
        "anime4k" => Ok(Upscaler::Anime4k),
        "cunny" => Ok(Upscaler::Cunny),
        "artcnn" => Ok(Upscaler::Artcnn),
        "k7" => Ok(Upscaler::K7),
        _ => Err("unknown upscaling filter".into()),
    }
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playback {
    scale: String,
    volume: u8,
    muted: bool,
    toggle: bool,
    /// Android always pauses in the background (AND-APP-3).
    #[cfg_attr(any(target_os = "android", target_os = "ios"), allow(dead_code))]
    pub(crate) pause_in_background: bool,
    discs: Vec<std::path::PathBuf>,
    font_files: Vec<std::path::PathBuf>,
    font_families: std::collections::BTreeMap<String, String>,
    smoothing: Option<bool>,
    /// Optional one-shot executable (relative to the game folder or absolute)
    /// launched in this game's Wine environment without changing its main exe.
    #[serde(default)]
    exe: Option<String>,
}
impl Playback {
    /// Prepare files off the UI thread before creating the native view.
    pub(crate) async fn prepare(
        &self,
        app: &tauri::AppHandle,
        exe: std::path::PathBuf,
        data_dir: std::path::PathBuf,
        codepage: u32,
        (setup, program_files): (Option<String>, Option<std::path::PathBuf>),
    ) -> Result<adv_shell::Options, String> {
        // Another program of the game runs on the game's drives, its folder
        // still the program drive (WINE-SES-12).
        let program = self.exe.as_ref().map(|target| {
            let candidate = std::path::Path::new(target);
            match exe.parent() {
                Some(folder) if candidate.is_relative() => folder.join(candidate),
                _ => candidate.to_path_buf(),
            }
        });
        match std::fs::metadata(program.as_deref().unwrap_or(&exe)) {
            _ if setup.is_some() => {}
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err("launch_missing_file".into()),
            Err(error) => {
                return Err(match error.kind() {
                    std::io::ErrorKind::NotFound => "launch_missing_file".into(),
                    std::io::ErrorKind::PermissionDenied => "launch_folder_access".into(),
                    _ => error.to_string(),
                });
            }
        }
        let mut options = self.options(exe, data_dir.clone(), codepage)?;
        options.program = program.map(|path| adv_shell::Program {
            image: adv_shell::ProgramImage::Host(path),
            arguments: String::new(),
        });
        // A pending install runs its disc's setup with no game yet; both put
        // `C:\Program Files` in the folder the player chose.
        if let Some(setup) = setup {
            options.run_alone(adv_shell::Program {
                image: adv_shell::ProgramImage::Guest(setup),
                arguments: String::new(),
            });
        }
        options.install_dir = program_files;
        crate::drive::apply(app, &mut options)?;
        if let Ok(cache_root) = app.path().app_cache_dir() {
            let gpu_root = cache_root.join("gpu");
            options.gpu_health_dir = Some(gpu_root.clone());
            if let Some(game_id) = data_dir.file_name() {
                options.d3d9_shader_cache = Some(gpu_root.join(game_id));
            }
        }
        let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
        tauri::async_runtime::spawn_blocking(move || crate::distribution::prepare_in(&app_data))
            .await
            .map_err(|e| e.to_string())??;
        Ok(options)
    }

    pub(crate) fn apply(&self, session: &Session) -> Result<(), String> {
        let scale = match self.scale.as_str() {
            "fit" => adv_shell::Scaling::Fit,
            "int" => adv_shell::Scaling::Integer,
            "stretch" => adv_shell::Scaling::Stretch,
            _ => return Err("unknown scaling mode".into()),
        };
        session.set_scaling(scale);
        session.set_volume(if self.muted { 0 } else { self.volume });
        session.set_fast_forward_toggle(self.toggle);
        Ok(())
    }

    /// Game options from the library entry and these playback settings.
    pub(crate) fn options(
        &self,
        exe: std::path::PathBuf,
        data_dir: std::path::PathBuf,
        codepage: u32,
    ) -> Result<adv_shell::Options, String> {
        let mut options = adv_shell::Options::new(exe, data_dir);
        options.codepage = Some(codepage);
        options.audio = true;
        if self.discs.len() > 4 {
            return Err("at most four disc images can be mounted".into());
        }
        options.cdroms = self
            .discs
            .iter()
            .enumerate()
            .map(|(i, p)| ((b'D' + i as u8) as char, p.clone()))
            .collect();
        options.font_files = self.font_files.clone();
        options.font_families = self.font_families.clone();
        options.font_smoothing = self.smoothing;
        Ok(options)
    }
}
