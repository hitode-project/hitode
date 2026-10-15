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
    #[cfg_attr(target_os = "android", allow(dead_code))]
    pub(crate) pause_in_background: bool,
    discs: Vec<std::path::PathBuf>,
    font_files: Vec<std::path::PathBuf>,
    font_families: std::collections::BTreeMap<String, String>,
    smoothing: Option<bool>,
}
impl Playback {
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
