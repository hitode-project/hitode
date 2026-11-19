//! Records the player's input as a `play_session.py` schedule: one JSON line per
//! window event, `{"at": seconds, "command": "..."}`, timed from process start.
//! The commands are the control channel's `input`, `window-input` and `text`,
//! which post the same messages, so a replay feeds the guest what the window did.
use std::io::Write;
use std::path::Path;
use std::time::Instant;

pub(crate) struct Recorder {
    out: std::io::LineWriter<std::fs::File>,
    started: Instant,
}

impl Recorder {
    /// The recorder `--record-input` or `ADV_RECORD_INPUT` asks for, if any.
    pub(crate) fn open(option: Option<&Path>, started: Instant) -> Result<Option<Self>, String> {
        let env = std::env::var_os("ADV_RECORD_INPUT").filter(|path| !path.is_empty());
        let Some(path) = option.or(env.as_deref().map(Path::new)) else {
            return Ok(None);
        };
        let file = std::fs::File::create(path)
            .map_err(|error| format!("input recording {}: {error}", path.display()))?;
        Ok(Some(Self {
            out: std::io::LineWriter::new(file),
            started,
        }))
    }

    /// Append `command`; a failed write ends the recording, never the game.
    pub(crate) fn record(recorder: &mut Option<Self>, command: std::fmt::Arguments<'_>) {
        let Some(this) = recorder else { return };
        let at = this.started.elapsed().as_secs_f64();
        let line = super::json(&command.to_string());
        if let Err(error) = writeln!(this.out, "{{\"at\": {at:.3}, \"command\": {line}}}") {
            log::warn!("input recording stopped: {error}");
            *recorder = None;
        }
    }
}
