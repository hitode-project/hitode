//! A program on the Wine userland (issue #58). The player keeps its execution
//! owner between games; the CLI selects it with `--wine DIST`.
//! Headless, it runs on this thread; with a window, it is the
//! execution worker behind the same UI as the built-in stack. Either way the
//! guest's windows reach the host natively: dialogs, the menu bar, IME text,
//! focus, activation, minimizing and closing, and the control channel.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Host as WineHost, Session, SessionOptions, Stop};
use adv_runtime::{DebugTarget, DialogEvent, DialogSet, Engine};
use adv_sync::WakeEvent;
use adv_sync::background::LocalSender;
use adv_sync::input::KeyState;

use super::{Finished, Options, control, frontend, json, save_frame, window, window_json};

/// How long the guest runs between looks at the host's events.
const SLICE: Duration = Duration::from_millis(20);
/// How long a closed window waits for the guest to exit by itself.
const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// How often a headless run presses one dialog's default button.
const AUTO_CLICKS: u32 = 3;

/// What the session gets from the shell before the process starts.
struct Host {
    background: LocalSender,
    gpu: Option<adv_device::gpu::Gpu>,
    frames: Option<(adv_runtime::FrameProducer, WakeEvent)>,
    pacer: Option<super::frame_pacer::HostPacer>,
}

fn start(
    environment: &mut WineHost,
    options: &Options,
    dist: &Path,
    wake: WakeEvent,
    keys: &Arc<KeyState>,
    host: Host,
) -> Result<Session, String> {
    let prefix = match &options.wine_prefix {
        Some(prefix) => prefix.clone(),
        None => options
            .data_dir
            .parent()
            .unwrap_or(Path::new("."))
            .join("wine"),
    };
    let prefix = std::path::absolute(prefix).map_err(|e| e.to_string())?;
    let debug = std::env::var("WINEDEBUG").unwrap_or_default();
    let codepage = match options.codepage {
        Some(codepage) => codepage,
        None => crate::detect_codepage(&options.exe).map_err(|e| e.to_string())?,
    };
    let config = Config {
        dist,
        prefix: &prefix,
        drive: options.wine_drive.as_deref(),
        locale: adv_runtime::wine::session::locale(codepage),
        fonts: adv_runtime::wine::session::HOST_FONTS,
        exe: &options.exe,
        width: options.width,
        height: options.height,
        jit: options.jit,
        debug: &debug,
        stdout: Some(libc::STDOUT_FILENO),
    };
    let gpu_wake = wake.clone();
    let codec_kernels = options.codec_kernels;
    let fonts = SessionOptions {
        fonts: adv_runtime::FontOverrides {
            families: options.font_families.clone(),
            antialiased: options.font_smoothing,
        },
        font_files: options.font_files.clone(),
        cdroms: options.cdroms.clone(),
        install_dir: options.install_dir.clone(),
    };
    environment.start(&config, wake.clone(), &fonts, move |machine| {
        Session::install_codec_kernels(machine, codec_kernels);
        machine.cpu.set_background_sender(host.background)?;
        options.configure_machine(machine, &wake, keys)?;
        if let Some(pacer) = host.pacer {
            if let Some(vsync) = &pacer.vsync {
                machine.ext.insert(vsync.clone());
            }
            machine.sys.frame_pacer = Some(Arc::new(pacer));
        }
        Session::configure_graphics(
            machine,
            options.d3d9_sm3,
            Some(
                options
                    .d3d9_shader_cache
                    .clone()
                    .unwrap_or_else(|| options.data_dir.join("artifacts/gpu")),
            ),
        );
        if let Some(gpu) = host.gpu {
            Session::attach_gpu(machine, gpu, gpu_wake)?;
        }
        if let Some((frames, ui)) = host.frames {
            let store = machine.ext.get_mut::<adv_device::surface::SurfaceStore>();
            store.set_frame_mailbox(frames, ui);
            // The UI takes published frames itself.
            machine.set_frame_handoff(false);
        }
        Ok(())
    })
}

#[cfg(target_os = "macos")]
pub(super) fn run(options: &Options, dist: &Path) -> Result<u8, String> {
    let wake = WakeEvent::new().map_err(|e| e.to_string())?;
    let mut background =
        adv_sync::background::Builder::new(wake.clone()).map_err(|e| e.to_string())?;
    let sender = background
        .sender_with_capacity_wake(8, 8, wake.clone())
        .map_err(|e| e.to_string())?;
    let mut service = background.start().map_err(|e| e.to_string())?;
    let result = (|| {
        let gpu = if options.headless_gpu {
            // As the built-in stack attaches a headless GPU.
            let gpu = pollster::block_on(adv_device::gpu::Gpu::request(options.gpu_software))
                .map_err(|e| e.to_string())?;
            gpu.context()
                .set_execution_wake(wake.clone())
                .map_err(|_| "GPU wake already installed")?;
            Some(gpu)
        } else {
            None
        };
        let host = Host {
            background: sender.into_local(),
            gpu,
            frames: None,
            pacer: None,
        };
        let debugger = super::debug::start(options, false, wake.clone());
        let keys = Arc::new(KeyState::default());
        let started = Instant::now();
        let session = start(
            &mut WineHost::default(),
            options,
            dist,
            wake.clone(),
            &keys,
            host,
        )?;
        let (code, session, _) = drive(options, session, None, &keys, &wake, debugger, started)?;
        let frame = session.frame();
        eprintln!(
            "wine: exit {code} after {:.2}s, {} frames presented",
            started.elapsed().as_secs_f64(),
            frame.frames
        );
        Ok(code)
    })();
    service.shutdown();
    result
}

/// The execution worker of a windowed run: the guest presents through the
/// UI's frame mailbox on the UI's GPU.
pub(super) fn run_execution(
    environment: &mut WineHost,
    options: &Options,
    dist: &Path,
    mut worker: frontend::Worker,
    wake: WakeEvent,
    keys: Arc<KeyState>,
    background: adv_sync::background::Sender,
) -> Result<Finished, String> {
    let pacer = super::frame_pacer::HostPacer {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        vsync: worker.vsync.take(),
        #[cfg(target_os = "android")]
        vsync: None,
        keys: keys.clone(),
        origin: Instant::now(),
    };
    let host = Host {
        background: background.into_local(),
        gpu: worker.gpu.take(),
        frames: Some((worker.take_frames(), worker.ui_wake.clone())),
        pacer: Some(pacer),
    };
    let started = Instant::now();
    let debugger = super::debug::start(options, true, wake.clone());
    let session = start(environment, options, dist, wake.clone(), &keys, host)?;
    let (exit_code, _, summary) = drive(
        options,
        session,
        Some(worker),
        &keys,
        &wake,
        debugger,
        started,
    )?;
    Ok(Finished { exit_code, summary })
}

/// Headless answers to dialogs: each one's default button, pressed once its
/// dialog has waited the configured time, at most [`AUTO_CLICKS`] times.
#[derive(Default)]
struct AutoDialog {
    seen: HashMap<u32, (Instant, u32)>,
}

impl AutoDialog {
    fn answer(&mut self, delay: Duration, session: &mut Session, now: Instant) {
        let set = session.dialogs();
        self.seen
            .retain(|hwnd, _| set.dialogs.iter().any(|dialog| dialog.target.hwnd == *hwnd));
        for dialog in set.dialogs.iter().filter(|dialog| dialog.enabled) {
            let (since, clicks) = self.seen.entry(dialog.target.hwnd).or_insert((now, 0));
            if *clicks >= AUTO_CLICKS || now < *since + delay * (*clicks + 1) {
                continue;
            }
            let buttons: Vec<_> = dialog
                .controls
                .iter()
                .filter(|control| {
                    control.target.parent == dialog.target.hwnd
                        && control.class.eq_ignore_ascii_case("Button")
                        && control.style & 0xf <= 1
                        && control.visible
                        && control.enabled
                })
                .collect();
            let pick = buttons
                .iter()
                .find(|button| button.style & 0xf == 1)
                .or_else(|| buttons.iter().find(|button| button.target.id == 1))
                .or_else(|| buttons.first());
            if let Some(button) = pick {
                *clicks += 1;
                log::info!(
                    "headless: pressing [{}] in window \"{}\"",
                    button.text,
                    dialog.text
                );
                session.dialog_event(DialogEvent::Click(button.target));
            }
        }
    }
}

struct Metrics {
    audio: super::audio::AudioOutput,
    since: Instant,
    first_frame_seconds: Option<f64>,
    start_frame: u64,
    performance: super::performance::Performance,
    gate: control::SceneGate,
}

impl Metrics {
    fn new(audio: super::audio::AudioOutput) -> Self {
        Self {
            audio,
            since: Instant::now(),
            first_frame_seconds: None,
            start_frame: 0,
            performance: super::performance::Performance::default(),
            gate: control::SceneGate::default(),
        }
    }
}

fn complete_gate(completion: control::GateCompletion, session: &mut Session, keys: &KeyState) {
    let release = completion.release.map(|key| {
        keys.set(key, false);
        (
            key,
            session.input_view().top != 0 && session.input(0x101, key, 0xc000_0001).is_ok(),
        )
    });
    control::acknowledge(match (completion.result, release) {
        (Ok(message), Some((key, true))) => {
            Ok(format!("{message} release={key} release_posted=true"))
        }
        (Ok(message), None) => Ok(format!("{message} release=none")),
        (Ok(message), Some((key, false))) => Err(format!(
            "{message} release={key} release_posted=false: no guest target window"
        )),
        (Err(message), release) => Err(format!(
            "{message} release={} release_posted={}",
            release.map_or_else(|| "none".into(), |(key, _)| key.to_string()),
            release.is_some_and(|(_, posted)| posted)
        )),
    });
}

fn observe_gate(
    session: &mut Session,
    metrics: &mut Metrics,
) -> Result<Option<control::GateCompletion>, String> {
    let before = metrics.gate.check_duration();
    if metrics
        .gate
        .needs_frame_pixels(session.presented_frames(), session.frame().frames)
    {
        let started = Instant::now();
        session.capture()?;
        metrics.gate.frame_capture(started.elapsed());
    }
    let completion = metrics.gate.observe(
        session.presented_frames(),
        session.frame().width,
        &session.frame().bgra,
        Instant::now(),
    );
    if let Some(budget) = &mut session.machine.sys.stats.frame_budget {
        budget.scene_check(metrics.gate.check_duration().saturating_sub(before));
    }
    Ok(completion)
}

/// The shared diagnostic report fields that this stack can measure.
#[derive(Default)]
struct Outcome {
    reason: &'static str,
    exit_code: Option<u32>,
    failure: Option<String>,
    screenshot_error: Option<String>,
}

fn report(
    options: &Options,
    session: &Session,
    metrics: &Metrics,
    started: Instant,
    outcome: Option<&Outcome>,
) -> String {
    let frame = session.frame();
    let sys = &session.machine.sys;
    let mut fields = vec![
        format!("\"exe\": {}", json(&options.exe.to_string_lossy())),
        format!(
            "\"backend\": {}",
            json(if options.jit { "jit" } else { "interpreter" })
        ),
        format!(
            "\"stop_reason\": {}",
            json(outcome.map_or("running", |outcome| outcome.reason))
        ),
        format!(
            "\"guest_exit_code\": {}",
            outcome
                .and_then(|outcome| outcome.exit_code)
                .map_or_else(|| "null".into(), |code| code.to_string())
        ),
        format!(
            "\"failure\": {}",
            outcome
                .and_then(|outcome| outcome.failure.as_deref())
                .map_or_else(|| "null".into(), json)
        ),
        format!(
            "\"screenshot_error\": {}",
            outcome
                .and_then(|outcome| outcome.screenshot_error.as_deref())
                .map_or_else(|| "null".into(), json)
        ),
        format!(
            "\"elapsed_seconds\": {:.6}",
            started.elapsed().as_secs_f64()
        ),
        format!(
            "\"first_frame_seconds\": {}",
            metrics
                .first_frame_seconds
                .map_or_else(|| "null".into(), |seconds| format!("{seconds:.6}"))
        ),
        format!(
            "\"frames\": {}",
            session.presented_frames() - metrics.start_frame
        ),
        format!("\"width\": {}, \"height\": {}", frame.width, frame.height),
        format!(
            "\"frame_fnv1a64\": \"{:016x}\"",
            super::checksum(&frame.bgra)
        ),
        format!("\"idle_waits\": {}", session.control_metrics().idle_waits),
        format!(
            "\"idle_wait_seconds\": {:.6}",
            session.control_metrics().wait_time.as_secs_f64()
        ),
        format!("\"guest_contexts\": {}", sys.contexts.len()),
        format!("\"host_calls\": {}", sys.host_calls),
        format!("\"tls_images\": {}", sys.tls_images.len()),
        format!(
            "\"time_skip\": {{\"enabled\": {}, \"jumps\": {}, \"skipped_guest_us\": {}}}",
            options.time_skip, sys.scheduling.active_time_skips, sys.scheduling.active_time_skip_us
        ),
        format!(
            "\"tier0_instructions\": {}",
            session
                .machine
                .cpu
                .tier0_instructions()
                .map_or_else(|| "null".into(), |count| count.to_string())
        ),
        session.machine.cpu.report_fields(),
        format!("\"performance\": {}", metrics.performance.json()),
        format!("\"audio\": {}", metrics.audio.json()),
        format!("\"scene_gate\": {}", metrics.gate.json()),
        format!("\"wine_boundaries\": {}", session.boundaries().json(24)),
    ];
    let mut codec = adv_runtime::wine::vorbisfile::stats(&session.machine).map_or_else(
        || "{\"provider\": \"guest\"}".into(),
        |s| format!(concat!("{{\"provider\": \"native\", \"opens\": {}, \"failed_opens\": {}, ",
            "\"clears\": {}, \"active_streams\": {}, \"decoded_packets\": {}, \"decoded_frames\": {}, ",
            "\"guest_callbacks\": {}, \"read_callbacks\": {}, \"seek_callbacks\": {}, \"tell_callbacks\": {}, ",
            "\"close_callbacks\": {}, \"callback_bytes\": {}, \"metadata_scans\": {}}}"),
            s.opens, s.failed_opens, s.clears, s.active_streams, s.decoded_packets, s.decoded_frames,
            s.guest_callbacks, s.read_callbacks, s.seek_callbacks, s.tell_callbacks, s.close_callbacks, s.callback_bytes, s.metadata_scans),
    );
    if let Some(selection) = session
        .machine
        .ext
        .try_get::<adv_runtime::wine::vorbisfile::Selection>()
    {
        codec.pop();
        codec.push_str(&format!(
            ", \"selection_reason\": {}}}",
            selection
                .reason
                .as_deref()
                .map_or_else(|| "null".into(), json)
        ));
    }
    fields.push(format!("\"codec\": {codec}"));
    if let Some(diagnostics) = &sys.stats.wait_diagnostics {
        fields.push(format!("\"wait_diagnostics\": {}", diagnostics.json()));
    }
    if let Some(budget) = &sys.stats.frame_budget {
        fields.push(format!("\"frame_budget\": {}", budget.json()));
    }
    format!("{{\n  {}\n}}\n", fields.join(",\n  "))
}

/// Adapter for the same debugger protocol and snapshots as the built-in stack.
struct DebugSession<'a> {
    session: &'a Session,
    options: &'a Options,
    metrics: &'a Metrics,
    started: Instant,
}

impl DebugTarget for DebugSession<'_> {
    type Backend = adv_runtime::Cpu;
    fn machine(&self) -> &adv_machine::Machine<Self::Backend> {
        &self.session.machine
    }
    fn guest_place(&self, address: u32) -> String {
        self.session.guest_place(address)
    }
    fn modules_report(&self) -> String {
        self.session.modules_report()
    }
    fn image_ranges(&self) -> Result<Vec<(u32, u32)>, String> {
        Ok(self
            .session
            .debug_modules()?
            .iter()
            .map(|m| (m.base, m.size))
            .collect())
    }
    fn profile_process_known(&self) -> bool {
        false
    }
    fn profile_place(&self, address: u32) -> String {
        format!("{address:#010x} (sample process unavailable)")
    }
    fn debug_counters(&self) -> [u64; 8] {
        self.session.debug_counters()
    }
    fn calls_report(&self, count: usize) -> String {
        format!(
            "pid {:#x}: NT/Unix and host-module calls (Wine PE calls are not traced)\n{}",
            self.session.machine.sys.pid,
            self.session.machine.sys.recent_calls_tail(count).join("\n")
        )
    }
    fn contexts_report(&self) -> String {
        self.session.contexts_report()
    }
    fn api_gaps_report(&self) -> String {
        "unavailable on the Wine stack: built-in API gap diagnostics; use Wine debug channels"
            .into()
    }
    fn report(&self) -> String {
        report(self.options, self.session, self.metrics, self.started, None)
    }
}

/// Run the guest and answer the host until it exits, the time limit passes
/// or a closed window's grace ends. Returns the exit code.
fn drive(
    options: &Options,
    mut session: Session,
    mut worker: Option<frontend::Worker>,
    keys: &Arc<KeyState>,
    wake: &WakeEvent,
    mut debugger: Option<super::debug::Debugger>,
    started: Instant,
) -> Result<(u8, Session, adv_report::RunSummary), String> {
    let mut metrics = Metrics::new(super::audio::AudioOutput::from_stream(
        options.audio,
        worker.is_none(),
        session.audio_stream(),
    ));
    #[cfg(target_os = "android")]
    {
        metrics.performance.execution_scheduling = Some(super::configure_execution_thread());
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if worker.is_some() {
        metrics.performance.execution_scheduling = Some(super::configure_execution_thread());
    }
    metrics.since = started;
    let mut control = options
        .control_stdin
        .then(|| control::Control::start(wake.clone(), keys.clone()))
        .transpose()?;
    let auto_delay = options
        .auto_dialog_ms
        .map(|ms| Duration::from_millis(u64::from(ms)));
    let mut auto = AutoDialog::default();
    let native = worker.is_some() || control.is_some() || auto_delay.is_some();
    let mut close_at: Option<Instant> = None;
    let mut minimized = false;
    let mut suspension = super::Suspension::default();
    let mut outcome = Outcome {
        reason: "host_window_closed",
        ..Default::default()
    };
    let result = (|| -> Result<(), String> {
        'run: loop {
            if worker.as_ref().is_some_and(frontend::Worker::stopped) {
                return Err("UI stopped while the guest was running".into());
            }
            while let Some(event) = worker
                .as_mut()
                .and_then(|worker| worker.next_input(&mut metrics.performance))
            {
                match event {
                    window::Event::Input {
                        message,
                        wparam,
                        lparam,
                    } => {
                        if let Some(debugger) = debugger.as_mut()
                            && super::debug_hotkey(keys, message, wparam)
                        {
                            debugger.hotkey(&DebugSession {
                                session: &session,
                                options,
                                metrics: &metrics,
                                started,
                            });
                            continue;
                        }
                        session.input(message, wparam, lparam)?;
                    }
                    #[cfg(host_view)]
                    window::Event::Text(text, reply) => {
                        let _ = reply.send(session.text(&text));
                    }
                    window::Event::Close => {
                        suspension.apply(&mut metrics.audio, None, Some(false), |paused| {
                            session.set_suspended(paused);
                        });
                        session.close();
                        close_at.get_or_insert_with(Instant::now);
                    }
                    #[cfg(host_view)]
                    window::Event::ForceClose => break 'run,
                    #[cfg(host_view)]
                    window::Event::AudioSuspended(sleeping) => {
                        suspension.apply(&mut metrics.audio, Some(sleeping), None, |paused| {
                            session.set_suspended(paused);
                        });
                    }
                    #[cfg(host_view)]
                    window::Event::Paused(value) => {
                        suspension.apply(&mut metrics.audio, None, Some(value), |paused| {
                            session.set_suspended(paused);
                        });
                    }
                    #[cfg(host_view)]
                    window::Event::Volume(percent) => {
                        metrics.audio.set_volume(percent);
                    }
                    window::Event::Repaint => {}
                    window::Event::FocusLost => {
                        session.release_input();
                    }
                    window::Event::Menu(event) => {
                        session.menu_event(event);
                    }
                    window::Event::Dialog(event) => {
                        session.dialog_event(event);
                    }
                }
            }
            if native {
                session.refresh_ui();
            }
            if let Some(controller) = control.as_mut() {
                let commands = if metrics.gate.frozen() {
                    controller.drain_frozen(true)
                } else {
                    controller.drain()
                };
                for command in commands {
                    if let control::Command::AwaitFrame(spec) = command {
                        let hold = spec.hold;
                        let result = if !options.headless {
                            Err("await-frame requires headless execution".into())
                        } else if hold.is_some() && session.input_view().top == 0 {
                            Err("await-frame hold has no guest target window".into())
                        } else {
                            metrics
                                .gate
                                .arm(spec, session.presented_frames(), Instant::now())
                        };
                        match result {
                            Err(error) => control::acknowledge(Err(error)),
                            Ok(()) => {
                                if let Some(key) = hold {
                                    keys.set(key, true);
                                    session.input(0x100, key, 1)?;
                                }
                            }
                        }
                        continue;
                    }
                    let close = matches!(command, control::Command::Close);
                    let result = match command {
                        control::Command::Resume => {
                            metrics.gate.resume(Instant::now()).map(|_| "resume".into())
                        }
                        control::Command::Report(path) => {
                            session.capture()?;
                            let snapshot = report(options, &session, &metrics, started, None);
                            super::write_report(&path, &snapshot, &session.machine.cpu)
                                .map(|_| format!("report {}", path.display()))
                        }
                        control::Command::ResetMetrics => {
                            session.reset_metrics();
                            let serial = session.presented_frames();
                            metrics.performance.reset(serial);
                            metrics.start_frame = serial;
                            metrics.since = Instant::now();
                            metrics.first_frame_seconds = None;
                            Ok(format!("reset-metrics start_frame={serial}"))
                        }
                        command => answer(&mut session, command),
                    };
                    control::acknowledge(result);
                    if close {
                        suspension.apply(&mut metrics.audio, None, Some(false), |paused| {
                            session.set_suspended(paused);
                        });
                        metrics.gate.close(Instant::now());
                        close_at.get_or_insert_with(Instant::now);
                        if let Some(worker) = worker.as_mut() {
                            worker.hide();
                        }
                    }
                }
                controller.after_guest_step(!metrics.gate.frozen());
            }
            if let Some(delay) = auto_delay {
                auto.answer(delay, &mut session, Instant::now());
            }
            if let Some(completion) = observe_gate(&mut session, &mut metrics)? {
                complete_gate(completion, &mut session, keys);
            }
            let reason = if options
                .frames
                .is_some_and(|frames| session.presented_frames() >= frames)
            {
                Some("frame_limit")
            } else if options
                .seconds
                .is_some_and(|seconds| started.elapsed().as_secs_f64() >= seconds)
            {
                Some("time_limit")
            } else if close_at.is_some_and(|at| at.elapsed() >= CLOSE_GRACE) {
                Some("host_window_closed")
            } else {
                None
            };
            if let Some(reason) = reason {
                outcome.reason = reason;
                break;
            }
            if let Some(debugger) = debugger.as_mut() {
                debugger.serve(&DebugSession {
                    session: &session,
                    options,
                    metrics: &metrics,
                    started,
                });
            }
            if suspension.paused || metrics.gate.frozen() {
                if metrics.gate.frozen() {
                    metrics.audio.set_suspended(true);
                }
                if !control.as_ref().is_some_and(control::Control::has_pending) {
                    let wait = if metrics.gate.frozen() {
                        let limit = options.seconds.map(|seconds| {
                            Duration::from_secs_f64(seconds).saturating_sub(started.elapsed())
                        });
                        let closing = close_at.map(|at| CLOSE_GRACE.saturating_sub(at.elapsed()));
                        match (limit, closing) {
                            (Some(a), Some(b)) => Some(a.min(b)),
                            (a, b) => a.or(b),
                        }
                    } else {
                        None
                    };
                    wake.wait(wait).map_err(|e| e.to_string())?;
                }
                continue;
            }
            metrics.audio.set_suspended(suspension.sleeping);
            metrics.audio.update();
            let run_before = session.control_metrics().run_time;
            let deadline = Instant::now()
                + metrics
                    .gate
                    .remaining(Instant::now())
                    .unwrap_or(SLICE)
                    .min(SLICE);
            let stop = if metrics.gate.pending() || options.frames.is_some() {
                session.run_ui(deadline)
            } else {
                session.run(deadline)
            };
            let serial = session.presented_frames();
            if let Some(debugger) = debugger.as_mut() {
                debugger.after_step(
                    session.control_metrics().run_time - run_before,
                    serial,
                    &DebugSession {
                        session: &session,
                        options,
                        metrics: &metrics,
                        started,
                    },
                );
            }
            metrics
                .performance
                .step(session.control_metrics().run_time - run_before, serial);
            if serial > metrics.start_frame && metrics.first_frame_seconds.is_none() {
                metrics.first_frame_seconds = Some(metrics.since.elapsed().as_secs_f64());
            }
            if let Some(completion) = observe_gate(&mut session, &mut metrics)? {
                complete_gate(completion, &mut session, keys);
            }
            match stop {
                Stop::Paused => {}
                Stop::Exited(code) => {
                    outcome.reason = "guest_exit";
                    outcome.exit_code = Some(code);
                    break;
                }
                Stop::Failed(error) => return Err(error),
            }
            if let Some(worker) = worker.as_mut() {
                let bar = session.menu_bar();
                if worker.needs_menu(bar.key) {
                    worker.publish_menu(bar);
                }
                worker.publish_dialogs(session.dialogs());
                // A guest minimizing itself (a boss key) minimizes the host
                // window; restoring that window restores the guest.
                let now_minimized = session.minimized();
                if now_minimized && !minimized {
                    worker.minimize();
                }
                minimized = now_minimized;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        outcome.reason = "guest_failure";
        outcome.failure = Some(error);
    }
    if let Some(completion) = metrics.gate.abort(
        "guest stopped before target",
        session.presented_frames(),
        Instant::now(),
    ) {
        complete_gate(completion, &mut session, keys);
    }
    session.machine.sys.trace.flush();
    session
        .machine
        .sys
        .schedule_trace
        .flush()
        .map_err(|e| e.to_string())?;
    if let Err(error) = session.machine.cpu.persist() {
        eprintln!("warning: {error}");
    }
    if session.presented_frames() > 0 {
        outcome.screenshot_error = session.capture().err();
    }
    if outcome.screenshot_error.is_none() {
        outcome.screenshot_error = options
            .screenshot
            .as_ref()
            .and_then(|path| save_frame(path, session.frame()).err());
    }
    if let Some(path) = &options.report {
        let snapshot = report(options, &session, &metrics, started, Some(&outcome));
        super::write_report(path, &snapshot, &session.machine.cpu)?;
    }
    options.write_profiles(&session.machine)?;
    if let Some(error) = &outcome.failure {
        eprintln!("wine: {error}");
    }
    let code = if outcome.failure.is_some() {
        1
    } else {
        outcome.exit_code.unwrap_or(0) as u8
    };
    let summary = adv_report::RunSummary {
        failure: super::report_failure(
            outcome.failure.as_deref(),
            outcome.exit_code,
            session
                .machine
                .sys
                .latest_cpu_fault
                .as_ref()
                .map(super::cpu_fault),
            started.elapsed(),
        ),
        perf: adv_report::Perf::from_samples(metrics.performance.frame_samples()),
        ..adv_report::RunSummary::default()
    };
    if let Some(error) = outcome.screenshot_error {
        return Err(error);
    }
    Ok((code, session, summary))
}

fn position(x: i16, y: i16) -> u32 {
    u32::from(x as u16) | u32::from(y as u16) << 16
}

/// One control command against the session.
fn answer(session: &mut Session, command: control::Command) -> Result<String, String> {
    use control::Command;
    let targeted = |session: &Session| session.input_view().top != 0;
    let post = |session: &mut Session, message, wparam, lparam| {
        targeted(session) && session.input(message, wparam, lparam).is_ok()
    };
    match command {
        Command::Click(x, y) => {
            let at = position(x, y);
            let posted = [(0x200, 0), (0x201, 1), (0x202, 0)]
                .into_iter()
                .filter(|&(message, wparam)| post(session, message, wparam, at))
                .count();
            if posted == 3 {
                Ok(format!("click {x} {y}"))
            } else {
                Err("click has no guest target window".into())
            }
        }
        Command::MouseDown(x, y)
        | Command::MouseUp(x, y)
        | Command::RightMouseDown(x, y)
        | Command::RightMouseUp(x, y) => {
            let right = matches!(
                command,
                Command::RightMouseDown(..) | Command::RightMouseUp(..)
            );
            let pressed = matches!(
                command,
                Command::MouseDown(..) | Command::RightMouseDown(..)
            );
            let buttons = match (pressed, right) {
                (false, _) => 0,
                (true, false) => 1,
                (true, true) => 2,
            };
            let message = match (right, pressed) {
                (false, true) => 0x201,
                (false, false) => 0x202,
                (true, true) => 0x204,
                (true, false) => 0x205,
            };
            let at = position(x, y);
            if post(session, 0x200, buttons, at) && post(session, message, buttons, at) {
                Ok(format!(
                    "{}mouse-{} {x} {y}",
                    if right { "right-" } else { "" },
                    if pressed { "down" } else { "up" }
                ))
            } else {
                Err("mouse button has no guest target window".into())
            }
        }
        Command::Move(x, y) => {
            if post(session, 0x200, 0, position(x, y)) {
                Ok(format!("move {x} {y}"))
            } else {
                Err("mouse move has no guest target window".into())
            }
        }
        Command::Key(key) => {
            if post(session, 0x100, key, 1) && post(session, 0x101, key, 0xc000_0001) {
                Ok(format!("key {key}"))
            } else {
                Err("key has no guest target window".into())
            }
        }
        Command::KeyDown(key) | Command::KeyUp(key) => {
            let pressed = matches!(command, Command::KeyDown(_));
            let (message, lparam) = if pressed {
                (0x100, 1)
            } else {
                (0x101, 0xc000_0001)
            };
            if post(session, message, key, lparam) {
                Ok(format!("key-{} {key}", if pressed { "down" } else { "up" }))
            } else {
                Err("key has no guest target window".into())
            }
        }
        Command::Text(text) => {
            let typed = session.text(&text);
            let total = text.chars().count();
            if typed == total {
                Ok(format!("text {typed}"))
            } else {
                Err(format!("text posted {typed} of {total} characters"))
            }
        }
        Command::Activate(active) => {
            if session.activate(active) {
                Ok(format!("activate {}", u8::from(active)))
            } else {
                Err("activation has no guest target window".into())
            }
        }
        Command::Windows => {
            let view = session.input_view();
            Ok(format!(
                "windows {{\"frames\": {}, \"top\": {}, \"focus\": {}, \"active\": {}, \"capture\": {}, \"windows\": [{}]}}",
                session.presented_frames(),
                view.top,
                view.focus,
                view.active,
                view.capture,
                view.windows
                    .iter()
                    .map(window_json)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
        Command::Hit(x, y) => Ok(format!(
            "hit {}",
            session
                .window_at(x.into(), y.into())
                .map_or_else(|| "null".into(), |window| window_json(&window))
        )),
        Command::Dialogs => Ok(format!(
            "dialogs {}",
            json(&format!("{:?}", session.dialogs()))
        )),
        Command::Dialog(id) | Command::DialogTrack(id, _) => {
            let set = session.dialogs();
            let found: Vec<_> = usable(&set)
                .flat_map(|dialog| dialog.controls.iter())
                .filter(|control| control.target.id == id && control.visible && control.enabled)
                .map(|control| control.target)
                .collect();
            let event = match command {
                Command::DialogTrack(_, value) => found.first().map(|&target| DialogEvent::Track {
                    target,
                    value,
                    code: 4,
                }),
                _ => found.first().map(|&target| DialogEvent::Click(target)),
            };
            match event {
                Some(event) if found.len() == 1 && session.dialog_event(event.clone()) => {
                    Ok(format!("dialog {id}"))
                }
                _ => Err(format!(
                    "dialog control {id} is missing, ambiguous or disabled"
                )),
            }
        }
        Command::DialogClose => {
            let set = session.dialogs();
            let found: Vec<_> = usable(&set).map(|dialog| dialog.target).collect();
            if found.len() == 1 && session.dialog_event(DialogEvent::Close(found[0])) {
                Ok("dialog-close".into())
            } else {
                Err("dialog-close requires one visible enabled dialog".into())
            }
        }
        Command::Menus => {
            fn visit(items: &[adv_runtime::MenuItem], path: &str, rows: &mut Vec<String>) {
                for item in items {
                    let label = format!("{path}/{}", String::from_utf16_lossy(&item.text));
                    if item.children.is_empty() {
                        rows.push(format!("{}:{}:{}", item.command.id, item.flags, label));
                    } else {
                        visit(&item.children, &label, rows);
                    }
                }
            }
            let bar = session.menu_bar();
            let mut rows = Vec::new();
            visit(&bar.items, "", &mut rows);
            Ok(format!(
                "menus enabled={} {}",
                bar.key.enabled,
                json(&rows.join(" | "))
            ))
        }
        Command::Menu(id) => {
            fn find(
                items: &[adv_runtime::MenuItem],
                id: u32,
                found: &mut Vec<adv_runtime::MenuCommand>,
            ) {
                for item in items {
                    if item.children.is_empty() && item.command.id == id {
                        found.push(item.command);
                    }
                    find(&item.children, id, found);
                }
            }
            let mut found = Vec::new();
            find(&session.menu_bar().items, id, &mut found);
            if found.len() == 1 && session.menu_event(adv_runtime::MenuEvent::Command(found[0])) {
                Ok(format!("menu {id}"))
            } else {
                Err(format!(
                    "menu {id} is missing, ambiguous, detached or disabled"
                ))
            }
        }
        Command::Capture(path) => {
            session.capture()?;
            save_frame(&path, session.frame()).map(|_| format!("capture {}", path.display()))
        }
        Command::Close => {
            session.close();
            Ok("close".into())
        }
        Command::Error(error) => Err(error),
        Command::Memory(address, len) => session.inspect_memory(address, len).map(|bytes| {
            let hex = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            format!("memory {address:#010x} {hex}")
        }),
        Command::Report(_) | Command::ResetMetrics | Command::AwaitFrame(_) | Command::Resume => {
            unreachable!("run-loop command")
        }
    }
}

fn usable(set: &DialogSet) -> impl Iterator<Item = &adv_runtime::DialogWindow> {
    set.dialogs
        .iter()
        .filter(|dialog| dialog.visible && dialog.enabled)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    // spec: WINE-UI-5, WINE-SES-7
    #[test]
    #[ignore = "subprocess entry for execution limit reports"]
    fn limits_child() {
        let root = std::env::current_dir().unwrap();
        let dist = PathBuf::from(std::env::var_os("ADV_WINE_DIST").unwrap());
        let frames = root.join("frames").exists();
        let mut options = Options::new(
            root.join(if frames { "d3d9.exe" } else { "popup_menu.exe" }),
            root.join("data"),
        );
        options.headless = true;
        options.codepage = Some(1252);
        options.wine_prefix = Some(root.join("prefix"));
        options.frames = frames.then_some(1);
        options.headless_gpu = frames;
        options.seconds = Some(if frames { 20. } else { 0.1 });
        options.report = Some(root.join("final.json"));
        options.screenshot = frames.then(|| root.join("final.png"));
        if root.join("failure").exists() {
            let wake = WakeEvent::new().unwrap();
            let mut background = adv_sync::background::Builder::new(wake.clone()).unwrap();
            let sender = background
                .sender_with_capacity_wake(8, 8, wake.clone())
                .unwrap();
            let mut service = background.start().unwrap();
            let keys = Arc::new(KeyState::default());
            let host = Host {
                background: sender.into_local(),
                gpu: None,
                frames: None,
                pacer: None,
            };
            let mut session = start(
                &mut WineHost::default(),
                &options,
                &dist,
                wake.clone(),
                &keys,
                host,
            )
            .unwrap();
            session.machine.set_execution_budget(Some(Duration::ZERO));
            let (code, _, summary) =
                drive(&options, session, None, &keys, &wake, None, Instant::now()).unwrap();
            assert_eq!(code, 1);
            assert!(summary.failure.is_some());
            service.shutdown();
        } else {
            assert_eq!(run(&options, &dist).unwrap(), 0);
        }
    }

    // spec: WINE-UI-5, WINE-SES-7
    #[test]
    fn final_reports_cover_limits_and_failure() {
        let (Some(_), Some(probes)) = (
            std::env::var_os("ADV_WINE_DIST"),
            std::env::var_os("ADV_WINE_PROBES"),
        ) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("adv-wine-limits-{}", std::process::id()));
        for mode in ["time", "frames", "failure"] {
            let frames = mode == "frames";
            let directory = root.join(mode);
            std::fs::create_dir_all(&directory).unwrap();
            if frames {
                std::fs::write(directory.join("frames"), "1").unwrap();
            } else if mode == "failure" {
                std::fs::write(directory.join("failure"), "1").unwrap();
            }
            let exe = if frames { "d3d9.exe" } else { "popup_menu.exe" };
            std::fs::copy(Path::new(&probes).join(exe), directory.join(exe)).unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "wine::tests::limits_child",
                    "--nocapture",
                ])
                .current_dir(&directory)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let report = std::fs::read_to_string(directory.join("final.json")).unwrap();
            let reason = match mode {
                "frames" => "frame_limit",
                "failure" => "guest_failure",
                _ => "time_limit",
            };
            assert!(
                report.contains(&format!("\"stop_reason\": \"{reason}\"")),
                "{report}"
            );
            assert!(report.contains("\"guest_exit_code\": null"), "{report}");
            if frames {
                assert!(report.contains("\"frames\": 1,"), "{report}");
            }
            if frames {
                assert!(
                    std::fs::metadata(directory.join("final.png"))
                        .unwrap()
                        .len()
                        > 0
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    // spec: WINE-UI-1, WINE-UI-2, WINE-UI-3, WINE-UI-4, WINE-UI-5, WINE-UI-6, WINE-SES-7
    #[test]
    #[ignore = "subprocess entry for the headless control test"]
    fn headless_control_child() {
        let root = std::env::current_dir().unwrap();
        let dist = std::env::var_os("ADV_WINE_DIST").unwrap();
        let mut options = Options::new(root.join("popup_menu.exe"), root.join("data"));
        options.wine = Some(dist.clone().into());
        options.wine_prefix = Some(root.join("prefix"));
        options.headless = true;
        options.debug = true;
        options.control_stdin = true;
        options.codepage = Some(1252);
        options.seconds = Some(60.);
        options.report = Some(root.join("final.json"));
        options.host_profile = Some(root.join("host.csv"));
        options.schedule_profile = Some(root.join("schedule.tsv"));
        options.profile = true;
        assert_eq!(run(&options, Path::new(&dist)).unwrap(), 0);
    }

    // spec: WINE-UI-1, WINE-UI-2, WINE-UI-3, WINE-UI-4, WINE-UI-5, WINE-UI-6, WINE-SES-7
    #[test]
    fn headless_wine_control() {
        let (Some(_dist), Some(probes)) = (
            std::env::var_os("ADV_WINE_DIST"),
            std::env::var_os("ADV_WINE_PROBES"),
        ) else {
            eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
            return;
        };
        let root = std::env::temp_dir().join(format!("adv-wine-control-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::copy(
            Path::new(&probes).join("popup_menu.exe"),
            root.join("popup_menu.exe"),
        )
        .unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "wine::tests::headless_control_child",
                "--nocapture",
            ])
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                send.send(line.unwrap()).unwrap();
            }
        });
        let mut transcript = String::new();
        let mut next = || {
            let line = receive
                .recv_timeout(Duration::from_secs(90))
                .expect("headless control output");
            transcript.push_str(&line);
            transcript.push('\n');
            line
        };
        let address = loop {
            let line = next();
            if let Some(address) = line.strip_prefix("memory=") {
                break address.to_owned();
            }
        };
        let socket = std::fs::read_dir(super::super::debug::socket_dir())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("{}-", child.id()))
            })
            .expect("Wine debugger socket");
        let mut debug = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        debug
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut debug_reader = BufReader::new(debug.try_clone().unwrap());
        let mut debug_command = |text: &str| {
            writeln!(debug, "test {text}").unwrap();
            let mut reply = String::new();
            debug_reader.read_line(&mut reply).unwrap();
            assert!(reply.contains("\"id\":\"test\""), "{reply}");
            assert!(reply.contains("\"ok\":true"), "{text}: {reply}");
            reply
        };
        {
            let mut command = |text: &str| {
                writeln!(input, "{text}").unwrap();
                input.flush().unwrap();
                loop {
                    let line = next();
                    if line.starts_with("control ") {
                        break line;
                    }
                }
            };
            loop {
                let menus = command("menus");
                if menus.contains("42:0:/More/Nested choice") {
                    assert!(menus.contains("enabled=true"), "{menus}");
                    break;
                }
            }
            assert_eq!(
                command(&format!("memory 0x{address} 4")),
                format!("control ok memory 0x{} 19283746", address.to_lowercase())
            );
            assert!(command("memory 0 4").starts_with("control error "));
            let report_path = root.join("before.json");
            assert!(
                command(&format!("report {}", report_path.display()))
                    .starts_with("control ok report ")
            );
            let report = std::fs::read_to_string(&report_path).unwrap();
            let hash = report
                .split_once("\"frame_fnv1a64\": \"")
                .unwrap()
                .1
                .split('"')
                .next()
                .unwrap();
            let latched = command(&format!("await-frame {hash} 2000 current"));
            assert!(
                latched.starts_with("control ok await-frame hash="),
                "{latched}"
            );
            assert!(latched.contains("frozen=true"), "{latched}");
            // Boundary requests must still be served while the frame gate freezes the guest.
            assert!(debug_command("info").contains("stack=wine"));
            assert!(debug_command("modules").contains("popup_menu.exe"));
            assert!(debug_command("threads").contains("tid 0x"));
            assert!(debug_command("stack 0 8").contains("context 0 esp"));
            assert!(debug_command(&format!("mem 0x{address} 4")).contains("19 28 37 46"));
            assert!(debug_command(&format!("dis 0x{address} 1")).contains("pid 0x"));
            assert!(debug_command("calls 8").contains("NT/Unix"));
            assert!(debug_command("gaps").contains("unavailable on the Wine stack"));
            debug_command("slow reset");
            debug_command("slow");
            assert!(debug_command("report").contains("scene_gate"));
            assert!(debug_command("timeline").contains("busy_us"));
            debug_command("samples 1");
            debug_command("mark wine-test");
            let snapshot = debug_command("snap wine-test");
            assert!(snapshot.contains("== modules"));
            assert!(snapshot.contains("popup_menu.exe"));
            let snapshots = root.join("data/debug");
            assert!(std::fs::read_dir(&snapshots).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with("-wine-test.txt")
            }));
            let frozen_path = root.join("frozen.json");
            command(&format!("report {}", frozen_path.display()));
            let frozen = std::fs::read_to_string(&frozen_path).unwrap();
            assert!(frozen.contains("\"frozen\":true"));
            assert_eq!(
                report.split_once("\"frames\": ").unwrap().1.lines().next(),
                frozen.split_once("\"frames\": ").unwrap().1.lines().next()
            );
            assert!(command("reset-metrics").starts_with("control ok reset-metrics start_frame="));
            let reset_path = root.join("reset.json");
            command(&format!("report {}", reset_path.display()));
            let reset = std::fs::read_to_string(&reset_path).unwrap();
            assert!(reset.contains("\"frames\": 0,"), "{reset}");
            assert!(reset.contains("\"idle_waits\": 0,"), "{reset}");
            assert!(
                reset.contains("\"idle_wait_seconds\": 0.000000,"),
                "{reset}"
            );
            assert!(reset.contains("\"first_frame_seconds\": null,"), "{reset}");
            assert!(reset.contains("\"frame_observations\": 0"), "{reset}");
            for unavailable in ["api", "codec_kernels", "graphics_memory"] {
                assert!(
                    !reset.contains(&format!("\"{unavailable}\":")),
                    "{unavailable}"
                );
            }
            assert_eq!(command("resume"), "control ok resume");
            assert_eq!(command("menu 42"), "control ok menu 42");
        }
        loop {
            if next().starts_with("picked=42 ") {
                break;
            }
        }
        writeln!(input, "key 27").unwrap();
        input.flush().unwrap();
        drop(input);
        reader.join().unwrap();
        while let Ok(line) = receive.try_recv() {
            transcript.push_str(&line);
            transcript.push('\n');
        }
        assert!(child.wait().unwrap().success(), "{transcript}");
        assert!(
            !socket.exists(),
            "debugger socket remained after the session ended"
        );
        assert!(
            transcript.contains("picked=42 initialized=2 commands=0 syscommands=0"),
            "{transcript}"
        );
        assert!(
            transcript.contains("cancelled=0 initialized=0 commands=0 syscommands=0"),
            "{transcript}"
        );
        let final_report = std::fs::read_to_string(root.join("final.json")).unwrap();
        assert!(
            final_report.contains("\"stop_reason\": \"guest_exit\""),
            "{final_report}"
        );
        assert!(
            final_report.contains("\"guest_exit_code\": 0"),
            "{final_report}"
        );
        assert!(final_report.contains("\"failure\": null"), "{final_report}");
        for path in ["final.cpu.tsv", "host.csv", "schedule.tsv"] {
            assert!(
                std::fs::metadata(root.join(path)).unwrap().len() > 0,
                "{path}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
