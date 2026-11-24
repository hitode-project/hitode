//! Game-run types and bookkeeping shared by the macOS and Android hosts.
use crate::library::{self, LastRun, Library};
use adv_shell::Session;
use serde::Serialize;
use std::{sync::Mutex, time::Instant};
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
        /// What a script wrote to the registry.
        #[serde(skip_serializing_if = "Option::is_none")]
        registry: Option<crate::drive::RegistryImport>,
    },
    Error {
        #[serde(rename = "gameId")]
        game_id: String,
        error: String,
        /// The failure kind and time bucket; never the error text.
        summary: adv_report::RunSummary,
    },
}

/// A running script: its console's input, and when it first showed a frame
/// or a dialog. Until then it runs behind the run page (WINE-SES-11).
pub(crate) struct Script {
    input: adv_shell::ConsoleInput,
    shown: Option<Instant>,
}
static SCRIPT: Mutex<Option<Script>> = Mutex::new(None);

/// The session of `script`, when prepared for one, has started.
pub(crate) fn started(script: Option<Script>) {
    if script.is_some() {
        *SCRIPT.lock().unwrap() = script;
    }
}

/// Sends a line to the running script, as Enter does.
#[tauri::command]
pub fn script_input(line: String) -> Result<(), String> {
    let mut script = SCRIPT.lock().unwrap();
    let script = script.as_mut().ok_or("no script is running")?;
    script.input.line(&line).map_err(|e| e.to_string())
}

/// The running script first showed something: the game view takes over.
pub(crate) fn shown(window: &WebviewWindow, id: &str) {
    if let Some(script) = SCRIPT.lock().unwrap().as_mut() {
        script.shown.get_or_insert_with(Instant::now);
    }
    let _ = window.emit("game_shown", id);
}

/// Records the play session in the library and emits `game_finished`. A
/// script counts only from when it showed something, and not at all if it
/// never did.
pub(crate) fn finished(
    window: &WebviewWindow,
    id: String,
    result: Result<adv_shell::Finished, String>,
    started: Instant,
) {
    let elapsed = started.elapsed();
    let played = match SCRIPT.lock().unwrap().take() {
        Some(script) => script.shown.map(|at| at.elapsed()),
        None => Some(elapsed),
    };
    let summary = match &result {
        Ok(finished) => finished.summary.clone(),
        Err(error) => adv_shell::error_summary(error, elapsed),
    };
    if let Some(played) = played
        && let Err(error) = window.state::<Mutex<Library>>().lock().unwrap().finished(
            &id,
            played.as_secs() as u32,
            LastRun {
                ended: library::now(),
                summary: Some(summary.clone()),
                error: result.as_ref().err().cloned(),
            },
        )
    {
        eprintln!("could not save play session: {error}");
    }
    let payload = match result {
        Ok(finished) => Finished::Exit {
            game_id: id,
            exit_code: finished.exit_code,
            summary,
            registry: finished
                .registry
                .map(|report| crate::drive::RegistryImport {
                    entries: report.entries,
                    normalized_wow64: report.normalized_wow64,
                }),
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
    /// Whether this run starts a script rather than the game or a program:
    /// it stays behind the run page until it shows something.
    pub(crate) fn script(&self) -> bool {
        self.exe.as_deref().is_some_and(|exe| {
            std::path::Path::new(exe)
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("bat") || ext.eq_ignore_ascii_case("cmd")
                })
        })
    }

    /// Prepare files off the UI thread before creating the native view.
    pub(crate) async fn prepare(
        &self,
        app: &tauri::AppHandle,
        id: &str,
        exe: std::path::PathBuf,
        data_dir: std::path::PathBuf,
        codepage: u32,
        (setup, program_files): (Option<String>, Option<std::path::PathBuf>),
    ) -> Result<(adv_shell::Options, Option<Script>), String> {
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
        // A script's console is the run page's (WINE-SES-11).
        let mut script = None;
        if self.script() {
            let window = app.clone();
            let game = id.to_owned();
            let (console, input) = adv_shell::Console::piped(codepage, move |text| {
                let _ = window.emit("script_output", (&game, text));
            })
            .map_err(|e| e.to_string())?;
            options.console = Some(console);
            script = Some(Script { input, shown: None });
        }
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
        Ok((options, script))
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

/// Waits off the UI thread until the game's thread has changed the disc.
pub async fn disc_changed(
    result: std::sync::mpsc::Receiver<Result<(), String>>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        result
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|_| "the game did not change the disc".to_string())?
    })
    .await
    .map_err(|error| error.to_string())?
}
